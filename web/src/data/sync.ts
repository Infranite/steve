/* The page's views (data/table.ts) kept in a single versioned ViewTable: loaded from `/api/snapshot`, then changed only
 * by the `patch` frames of the event stream (and reads by id). The stream continues from the snapshot's cursor, so no
 * patch after it is missed; a reset loads the snapshot again. Settled decisions are a page of their own (`['page', …]`),
 * never mixed into the table: a snapshot drops the hot views it no longer holds, and a page is not hot. */
import { QueryClient, useInfiniteQuery } from "@tanstack/react-query"
import { useCallback, useEffect, useSyncExternalStore } from "react"
import { api, revOf } from "@/api"
import { decisionText } from "@/i18n/decision"
import type { DecisionView, Envelope, ItemView, MessageView, PageOf, Patch, SessionView, View, ViewKind } from "@/gen"
import { HOT, ViewTable, type ChangeSet } from "./table"

export const qc = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, gcTime: Infinity, retry: false, refetchOnWindowFocus: false } } })

/** the kinds this page reads as views */
const KINDS: ViewKind[] = ["decision", "item", "session", "message"]
/** how far back 已处理 goes */
const SETTLED_DAYS = 7

/* a workspace switch starts a new scope: nothing of the old one is merged into it */
let scope = 0
export const scopeKey = () => scope
const settledKey = () => ["page", scope, "decision", "settled"] as const

const views = new ViewTable()
export const table = (): ViewTable => views
const history = () => qc.getQueryData<PageOf<DecisionView>>(settledKey())
export const hasMoreSettled = () => !!history()?.next

let settledVersion = 0
const settledListeners = new Set<() => void>()
const bumpSettled = () => {
  settledVersion++
  settledListeners.forEach((f) => f())
}
export const onSettled = (f: () => void) => { settledListeners.add(f); return () => { settledListeners.delete(f) } }
export const useSettledVersion = () => useSyncExternalStore(onSettled, () => settledVersion)

let displayed: { decisionVer: number; page: ReturnType<typeof history>; rows: DecisionView[] } | undefined
export function settled(): DecisionView[] {
  const decisionVer = views.kindVersion("decision"), page = history()
  if (displayed && displayed.decisionVer === decisionVer && displayed.page === page) return displayed.rows
  const rows = new Map((page?.views ?? []).map((v) => [v.id, v]))
  for (const [id] of rows) {
    if ((views.tomb.get(`decision:${id}`) ?? 0) > (page?.rev ?? 0)) rows.delete(id)
  }
  const since = Date.now() - SETTLED_DAYS * 86_400_000
  for (const view of views.viewsOf<DecisionView>("decision")) {
    const rev = views.rows.get(`decision:${view.id}`)!.rev
    if (rev < (page?.rev ?? 0) && rows.has(view.id)) continue
    if (view.status === "open") rows.delete(view.id)
    else if ((view.resolved ?? 0) > since) rows.set(view.id, view)
  }
  const sorted = [...rows.values()].sort((a, b) => (b.resolved ?? 0) - (a.resolved ?? 0) || b.id.localeCompare(a.id))
  displayed = { decisionVer, page, rows: sorted }
  return sorted
}

/* Fine-grained view subscriptions on the versioned ViewTable. */
const noop = () => {}
export const onViews = (f: () => void) => views.subscribeAll(f)
export const onViewKind = (kind: ViewKind, f: () => void) => views.subscribeKind(kind, f)
export const onViewKinds = (kinds: readonly ViewKind[], f: () => void) => views.subscribeKinds(kinds, f)
export const onViewGroup = (group: string, f: () => void) => views.subscribeGroup(group, f)
export const onEntityView = (kind: ViewKind, id: string, f: () => void) => views.subscribeEntity(kind, id, f)

const catalog = new Set<() => void>()
export const onCatalog = (f: () => void) => { catalog.add(f); return () => { catalog.delete(f) } }
const notifyCatalog = () => { catalog.forEach((f) => f()) }

function finishMerge(change: ChangeSet, allowCatalog = true) {
  if (!change.changed) return
  if (change.kinds.has("decision")) bumpSettled()
  if (allowCatalog && (change.kinds.has("item") || change.kinds.has("session") || change.kinds.has("decision"))) {
    notifyCatalog()
  }
}

/* the snapshot's ETag: a reload after a reset that finds nothing changed costs a 304 */
let etag: string | undefined
/** the stream's cursor of the last snapshot read */
let cursor: string | null = null
export const viewsCursor = () => cursor
const messagePageKey = () => ["page", scope, "message"] as const
type MessagePage = { next: string | null; pending: boolean }
const INITIAL_PAGE: MessagePage = { next: null, pending: false }
const messagePageListeners = new Set<() => void>()
const notifyMessagePage = () => { messagePageListeners.forEach((f) => f()) }
const onMessagePage = (f: () => void) => { messagePageListeners.add(f); return () => { messagePageListeners.delete(f) } }
export const messagePageState = () => qc.getQueryData<MessagePage>(messagePageKey()) ?? INITIAL_PAGE
export const useMessagePage = () => useSyncExternalStore(onMessagePage, messagePageState)
let pendingPage: { scope: number; request: Promise<boolean> } | undefined
export function loadMessagePage(signal?: AbortSignal): Promise<boolean> {
  const s = scope, key = messagePageKey(), { next } = messagePageState()
  if (pendingPage?.scope === s) return pendingPage.request
  if (!next || signal?.aborted) return Promise.resolve(false)
  const request = (async () => {
    qc.setQueryData(key, { next, pending: true }); notifyMessagePage()
    try {
      const page = await api.messagePage(next, signal)
      if (s !== scope || signal?.aborted) return false
      qc.setQueryData(key, { next: page.next, pending: false })
      finishMerge(views.mergeViews(page.rev, "message", page.views), false)
      notifyMessagePage()
      return true
    } finally {
      if (pendingPage?.scope === s) pendingPage = undefined
      if (s === scope) {
        if (messagePageState().pending) {
          qc.setQueryData(key, { next, pending: false })
          notifyMessagePage()
        }
      }
    }
  })()
  pendingPage = { scope: s, request }
  return request
}

/** Loads the hot set first. The settled page can arrive independently. */
const snapshotTimes: number[] = []
export const takeSnapshotTimes = () => snapshotTimes.splice(0)
export async function loadViews(): Promise<void> {
  const s = scope, started = performance.now()
  const snap = await api.snapshot(KINDS.join(","), etag)
  if (s !== scope) return
  if (snap) {
    etag = snap.etag; cursor = snap.snapshot.cursor
    const updatedPage = !!(snap.snapshot.views.message && messagePageState() === INITIAL_PAGE)
    if (updatedPage) {
      qc.setQueryData(messagePageKey(), { next: snap.snapshot.views.message!.next ?? null, pending: false })
    }
    finishMerge(views.mergeSnapshot(snap.snapshot.rev, snap.snapshot.views))
    if (updatedPage) notifyMessagePage()
    if (snapshotTimes.length < 500) snapshotTimes.push(performance.now() - started)
  }
}

export function resetViews() {
  qc.removeQueries({ queryKey: ["page", scope] })
  qc.removeQueries({ queryKey: ["detail", scope] })
  qc.removeQueries({ queryKey: ["live", scope] })
  qc.removeQueries({ queryKey: ["shell", scope] })
  qc.removeQueries({ queryKey: ["notifications", scope] })
  qc.removeQueries({ queryKey: ["tag-uses", scope] })
  qc.removeQueries({ queryKey: ["bot-memories", scope] })
  openLists.clear()
  workCache.clear()
  displayed = undefined
  scope++; etag = undefined; cursor = null
  views.reset()
  bumpSettled()
  notifyMessagePage()
  notifyCatalog()
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
  bumpSettled()
  notifyCatalog()
}

export function useSettledDecisions(enabled = true) {
  useEffect(() => { if (enabled && !history()) void loadSettled().catch(() => {}) }, [enabled, scope])
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
  const coldChanged = new Set<"item" | "session">()
  for (const op of frame.ops) {
    if (op.type === "decision" || op.type === "message") continue
    const kind = op.type
    const before = views.rows.get(`${kind}:${op.id}`)?.view
    if (op.op === "remove" || !op.view || !HOT[kind](op.view) || (before && !HOT[kind](before))) coldChanged.add(kind)
  }
  const change = views.mergePatch(frame.rev, frame.ops)
  finishMerge(change, frame.ops.some((op) => op.type !== "message"))
  if (applied.length < 500) applied.push(performance.now() - t0)
  for (const kind of coldChanged) void qc.invalidateQueries({ queryKey: coldKey(kind) })
}

/** After a command: until the views hold its revision (from its answer's `x-steve-rev`), then the answer. A patch
 * normally brings it within a frame; after a second, the views it changed are read by id instead. */
export async function settle<T>(answer: T, kind: ViewKind, ids: string[]): Promise<T> {
  const rev = revOf(answer)
  const ready = () => ids.every((id) =>
    Math.max(views.rows.get(`${kind}:${id}`)?.rev ?? 0, views.tomb.get(`${kind}:${id}`) ?? 0) >= rev)
  if (!rev || ready()) return answer
  const s = scope
  const caught = await new Promise<boolean>((done) => {
    const timer = setTimeout(() => { off(); done(false) }, 1000)
    const off = views.subscribeAll(() => { if (s !== scope || ready()) { clearTimeout(timer); off(); done(true) } })
  })
  if (!caught && s === scope && ids.length) {
    const r = await api.views(kind, ids).catch(() => null)
    if (r && s === scope) finishMerge(views.mergeViews(r.rev, kind, r.views, r.missing), kind !== "message")
  }
  return answer
}

/* ── selectors ── */
export const decisionView = (id: string) => views.viewOf<DecisionView>("decision", id)
const orderDecisions = (a: DecisionView, b: DecisionView) => a.created - b.created || a.id.localeCompare(b.id)
const openLists = new Map<string, { ver: number; rows: DecisionView[] }>()
function open(group: string) {
  const ver = views.groupVersion(group)
  const prior = openLists.get(group)
  if (prior?.ver === ver) return prior.rows
  const next = views.viewsOf<DecisionView>("decision", group).filter((d) => d.status === "open").sort(orderDecisions)
  const rows = prior && prior.rows.length === next.length && next.every((d, i) => d === prior.rows[i]) ? prior.rows : next
  openLists.set(group, { ver, rows })
  return rows
}
export const openDecisions = () => open("hot:decision")
export const decisionsForItem = (id: string) => open(`decision:item:${id}`)
export const decisionsForSession = (id: string) => open(`decision:session:${id}`)

export const messageView = (id: string) => views.viewOf<MessageView>("message", id)
export const messageViews = () => views.viewsOf<MessageView>("message")
export const itemView = (id: string) => views.viewOf<ItemView>("item", id)
export const sessionView = (id: string) => views.viewOf<SessionView>("session", id)
export const itemViews = () => views.viewsOf<ItemView>("item", "hot:item")
export const itemIds = () => views.index.get("item")
export const sessionViews = () => views.viewsOf<SessionView>("session", "hot:session")
export const sessionsForItem = (id: string) => views.viewsOf<SessionView>("session", `session:item:${id}`)

export function useViewTable(): number {
  return useSyncExternalStore(onViews, () => views.version)
}

export function useViewKind(kind: ViewKind): number {
  const subscribe = useCallback((cb: () => void) => views.subscribeKind(kind, cb), [kind])
  return useSyncExternalStore(subscribe, () => views.kindVersion(kind))
}

export function useViewKinds(kinds: readonly ViewKind[]): number {
  const key = kinds.join(",")
  const subscribe = useCallback((cb: () => void) => views.subscribeKinds(kinds, cb), [key]) // eslint-disable-line react-hooks/exhaustive-deps
  return useSyncExternalStore(subscribe, () => {
    let max = 0
    for (let i = 0; i < kinds.length; i++) {
      const v = views.kindVersion(kinds[i])
      if (v > max) max = v
    }
    return max
  })
}

export function useViewGroup(group: string | null | undefined): number {
  const subscribe = useCallback((cb: () => void) => group ? views.subscribeGroup(group, cb) : noop, [group])
  return useSyncExternalStore(subscribe, () => group ? views.groupVersion(group) : 0)
}

export function useEntityView<V extends View>(kind: ViewKind, id: string | null | undefined): V | undefined {
  const subscribe = useCallback((cb: () => void) => id ? views.subscribeEntity(kind, id, cb) : noop, [kind, id])
  return useSyncExternalStore(subscribe, () => id ? views.viewOf<V>(kind, id) : undefined)
}

export const useItemView = (id: string | null | undefined) => useEntityView<ItemView>("item", id)
export const useSessionView = (id: string | null | undefined) => useEntityView<SessionView>("session", id)
export const useDecisionView = (id: string | null | undefined) => useEntityView<DecisionView>("decision", id)
export const useMessageView = (id: string | null | undefined) => useEntityView<MessageView>("message", id)

export async function readViews(kind: ViewKind, ids: string[]) {
  const s = scope
  await Promise.all(Array.from({ length: Math.ceil(ids.length / 500) }, (_, i) => api.views(kind, ids.slice(i * 500, i * 500 + 500)).then((r) => {
    if (s === scope) finishMerge(views.mergeViews(r.rev, kind, r.views, r.missing), kind !== "message")
  })))
}

export async function readItemSessions(ids: string[]) {
  const s = scope
  const r = await api.itemSessions(ids)
  if (s === scope) finishMerge(views.mergeViews(r.rev, "session", r.views))
}

type HistoryView = ItemView | SessionView
const coldKey = (kind: "item" | "session") => ["page", scope, kind] as const
function useHistory<T extends HistoryView>(
  kind: "item" | "session",
  enabled: boolean,
  read: (before?: string) => Promise<PageOf<T>>,
  cold: (view: T) => boolean,
) {
  useViewKind(kind)
  const q = useInfiniteQuery({
    queryKey: coldKey(kind), queryFn: ({ pageParam }) => read(pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next ?? undefined, enabled,
  }, qc)
  const rows = new Map<string, T>()
  for (const page of q.data?.pages ?? []) for (const v of page.views) {
    if ((views.tomb.get(`${kind}:${v.id}`) ?? -1) >= page.rev) continue
    const current = views.rows.get(`${kind}:${v.id}`)
    if (current && current.rev >= page.rev) {
      const view = current.view as T
      if (cold(view)) rows.set(v.id, view)
    } else rows.set(v.id, v)
  }
  for (const v of views.viewsOf<T>(kind)) {
    if (cold(v) && (views.rows.get(`${kind}:${v.id}`)?.rev ?? -1) > (q.data?.pages[0]?.rev ?? Infinity)) rows.set(v.id, v)
  }
  return { rows: [...rows.values()], more: q.hasNextPage, load: q.fetchNextPage, pending: q.isPending, error: q.error }
}

export const useArchived = (enabled: boolean) => useHistory("item", enabled, api.archivedItems, (v) => !!v.archived)
export const useEnded = (enabled: boolean) => useHistory("session", enabled, api.endedSessions, (v) => v.status === "ended")

const onHotDecisions = (cb: () => void) => views.subscribeGroup("hot:decision", cb)
/** The inbox list subscribes only to open decisions; unchanged row views keep their identity across patches. */
export const useOpenDecisions = () => useSyncExternalStore(onHotDecisions, openDecisions)

type Blocker = { kind: string; question?: string; op_kind?: string; tag?: string | null }
const blockerOrder = ["failure", "unknown_effect", "permission", "acceptance", "approval", "question", "dependency", "paused", "delivery", "operation", "turn"]
function blockerOf(d: DecisionView): Blocker {
  const kind = d.kind === "accept" ? "acceptance" : d.kind === "fail" ? "failure"
    : d.subject_type === "tool_call" && d.kind === "approve" ? "permission"
    : d.kind === "approve" || d.kind === "propose" ? "approval" : "question"
  return { kind, question: decisionText(d.question).slice(0, 60) }
}

function workOf(id: string) {
  const item = itemView(id)
  const sessions = sessionsForItem(id)
  const active = sessions.filter((s) => s.status !== "ended")
  const isRunning = (s: SessionView) => s.status === "running" || s.status === "starting"
  const running = active.some(isRunning)
  const turn = active.some((s) => s.busy || isRunning(s))
  const newest = active.reduce<SessionView | undefined>((best, s) =>
    !best || s.no > best.no || (s.no === best.no && s.created > best.created) ? s : best, undefined)
  const decisions = decisionsForItem(id)
  const blockers: Blocker[] = []
  const closing = item?.state.kind === "closing" ? item.state : undefined
  const waiting = new Set<string>(closing?.waits)
  if (item && item.state.kind !== "closed") {
    blockers.push(...decisions.map(blockerOf), ...(item.op_blockers ?? []))
    if (item.waits?.includes("prerequisite")) blockers.push({ kind: "dependency" })
    if (item.state.kind === "paused") blockers.push({ kind: "paused" })
    if (closing && turn) { waiting.add("turn"); blockers.push({ kind: "turn" }) }
    if (waiting.has("delivery")) blockers.push({ kind: "delivery" })
    if (waiting.has("operation") && !blockers.some((b) => b.kind === "operation")) blockers.push({ kind: "operation" })
  }
  blockers.sort((a, b) => blockerOrder.indexOf(a.kind) - blockerOrder.indexOf(b.kind))
  return { running, active: newest, queued: newest?.queued ?? 0, decisions, blockers, waiting: [...waiting] }
}

const workCache = new Map<string, { ver: number; state: ReturnType<typeof workOf> }>()
const itemWorkVersion = (id: string) => Math.max(
  views.entityVersion("item", id),
  views.groupVersion(`session:item:${id}`),
  views.groupVersion(`decision:item:${id}`),
)

export function itemWork(id: string) {
  const ver = itemWorkVersion(id)
  const cached = workCache.get(id)
  if (cached && cached.ver === ver) return cached.state
  const state = workOf(id)
  workCache.set(id, { ver, state })
  return state
}

export function subscribeItemScope(id: string, cb: () => void): () => void {
  const offs = [
    views.subscribeEntity("item", id, cb),
    views.subscribeGroup(`session:item:${id}`, cb),
    views.subscribeGroup(`decision:item:${id}`, cb),
  ]
  return () => { for (const off of offs) off() }
}

export function useItemState(id: string) {
  const subscribe = useCallback((cb: () => void) => subscribeItemScope(id, cb), [id])
  return useSyncExternalStore(subscribe, () => itemWork(id))
}
