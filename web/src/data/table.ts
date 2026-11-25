/* The views table: every view this page holds (`steve_types::view` on the Hub), from the snapshot, patch frames,
 * reads by id and pages, merged by one rule: per (kind, id) the copy with the larger `rev` wins, a later one on a tie.
 * Mutates in place with monotonic versions per table, kind, group and entity, plus fine-grained subscriptions so
 * unrelated patches never clone the table, run JSON.stringify, or wake unrelated views.
 * Relative, type-only imports so node --test loads this file without a bundler. */
import type { ItemView } from "../gen/ItemView.ts"
import type { SessionView } from "../gen/SessionView.ts"
import type { DecisionView } from "../gen/DecisionView.ts"
import type { PatchOp } from "../gen/PatchOp.ts"
import type { SnapshotViews } from "../gen/SnapshotViews.ts"
import type { View } from "../gen/View.ts"
import type { ViewKind } from "../gen/ViewKind.ts"

export type Row = { view: View; rev: number }

export type ChangeSet = {
  changed: boolean
  kinds: Set<ViewKind>
  groups: Set<string>
  entities: Set<string>
}

/** The Hub's hot sets (steve-hub `projection/kinds.rs` `is_hot`): what a snapshot holds of each kind. */
export const HOT: Record<ViewKind, (v: View) => boolean> = {
  message: () => false,
  decision: (v) => (v as DecisionView).status === "open",
  item: (v) => !(v as ItemView).archived,
  session: (v) => (v as SessionView).status !== "ended",
}

const key = (kind: ViewKind, id: string) => `${kind}:${id}`
const kindOf = (k: string) => k.slice(0, k.indexOf(":")) as ViewKind

/** Fast structural equality for JSON-shaped views without serializing strings. */
export function deepEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true
  if (typeof a !== "object" || a === null || typeof b !== "object" || b === null) return false
  const aArr = Array.isArray(a)
  if (aArr !== Array.isArray(b)) return false
  if (aArr) {
    const xs = a as unknown[], ys = b as unknown[]
    if (xs.length !== ys.length) return false
    for (let i = 0; i < xs.length; i++) if (!deepEqual(xs[i], ys[i])) return false
    return true
  }
  const oa = a as Record<string, unknown>, ob = b as Record<string, unknown>
  let aCount = 0
  for (const k in oa) {
    if (!Object.hasOwn(oa, k)) continue
    const va = oa[k]
    if (va === undefined) continue
    aCount++
    if (!Object.hasOwn(ob, k) || !deepEqual(va, ob[k])) return false
  }
  let bCount = 0
  for (const k in ob) {
    if (Object.hasOwn(ob, k) && ob[k] !== undefined) bCount++
  }
  return aCount === bCount
}

const EMPTY_GROUPS: readonly string[] = []
function groups(kind: ViewKind, v: View): string[] {
  const out = [kind as string]
  if (HOT[kind](v)) out.push(`hot:${kind}`)
  if (kind === "session" || kind === "decision") {
    const d = v as SessionView | DecisionView
    if (d.item_id) out.push(`${kind}:item:${d.item_id}`)
    if ("session_id" in d && d.session_id) out.push(`decision:session:${d.session_id}`)
  }
  return out
}

function addListener<K>(map: Map<K, Set<() => void>>, k: K, fn: () => void): () => void {
  let set = map.get(k)
  if (!set) map.set(k, (set = new Set()))
  set.add(fn)
  return () => {
    set!.delete(fn)
    if (!set!.size) map.delete(k)
  }
}

export class ViewTable {
  readonly rows = new Map<string, Row>()
  readonly index = new Map<string, Set<string>>()
  /** removed entities: their key and the rev they were removed at */
  readonly tomb = new Map<string, number>()
  /** per kind, the rev of the last snapshot: a hot view at or before it that the snapshot did not hold is stale */
  readonly floor: Partial<Record<ViewKind, number>> = {}

  private ver = 0
  private resetVer = 0
  private readonly kindVer = new Map<ViewKind, number>()
  private readonly groupVer = new Map<string, number>()
  private readonly entityVer = new Map<string, number>()
  private readonly listCache = new Map<string, { ver: number; rows: View[] }>()

  private readonly allListeners = new Set<() => void>()
  private readonly kindListeners = new Map<ViewKind, Set<() => void>>()
  private readonly groupListeners = new Map<string, Set<() => void>>()
  private readonly entityListeners = new Map<string, Set<() => void>>()

  get version(): number { return this.ver }
  kindVersion(kind: ViewKind): number { return this.kindVer.get(kind) ?? this.resetVer }
  groupVersion(group: string): number { return this.groupVer.get(group) ?? this.resetVer }
  entityVersion(kind: ViewKind, id: string): number { return this.entityVer.get(key(kind, id)) ?? this.resetVer }

  subscribeAll(fn: () => void): () => void {
    this.allListeners.add(fn)
    return () => { this.allListeners.delete(fn) }
  }
  subscribeKind(kind: ViewKind, fn: () => void): () => void {
    return addListener(this.kindListeners, kind, fn)
  }
  subscribeKinds(kinds: readonly ViewKind[], fn: () => void): () => void {
    const offs = kinds.map((k) => this.subscribeKind(k, fn))
    return () => { for (const off of offs) off() }
  }
  subscribeGroup(group: string, fn: () => void): () => void {
    return addListener(this.groupListeners, group, fn)
  }
  subscribeEntity(kind: ViewKind, id: string, fn: () => void): () => void {
    return addListener(this.entityListeners, key(kind, id), fn)
  }

  private beginBatch(): ChangeSet {
    return { changed: false, kinds: new Set(), groups: new Set(), entities: new Set() }
  }

  private reindex(kind: ViewKind, id: string, before: View | undefined, after: View | undefined, changedGroups: Set<string>) {
    const prior = before ? groups(kind, before) : EMPTY_GROUPS
    const next = after ? groups(kind, after) : EMPTY_GROUPS
    for (let i = 0; i < prior.length; i++) {
      const g = prior[i]
      changedGroups.add(g)
      if (!next.includes(g)) {
        const ids = this.index.get(g)
        if (ids) {
          ids.delete(id)
          if (!ids.size) this.index.delete(g)
        }
      }
    }
    for (let i = 0; i < next.length; i++) {
      const g = next[i]
      changedGroups.add(g)
      if (!prior.includes(g)) {
        let ids = this.index.get(g)
        if (!ids) this.index.set(g, (ids = new Set()))
        ids.add(id)
      }
    }
  }

  private upsert(kind: ViewKind, id: string, view: View, rev: number, batch: ChangeSet) {
    const k = key(kind, id), had = this.rows.get(k)
    if (had && had.rev > rev) return
    if ((this.tomb.get(k) ?? -1) > rev) return
    if (HOT[kind](view) && rev < (this.floor[kind] ?? -1) && (!had || !HOT[kind](had.view))) {
      // The snapshot excludes this open copy. It still tells us that any earlier
      // settled copy was superseded, even when the later settlement has not arrived.
      this.remove(kind, id, rev, batch)
      return
    }
    const same = !!had && deepEqual(had.view, view)
    if (same && had.rev === rev) return
    if (!same) this.reindex(kind, id, had?.view, view, batch.groups)
    this.rows.set(k, { view: same ? had!.view : view, rev })
    if (this.tomb.has(k)) this.tomb.delete(k)
    batch.changed = true
    if (!same) {
      batch.kinds.add(kind)
      batch.entities.add(k)
    }
  }

  private remove(kind: ViewKind, id: string, rev: number, batch: ChangeSet) {
    const k = key(kind, id), had = this.rows.get(k)
    if (had && had.rev > rev) return
    if ((this.tomb.get(k) ?? -1) >= rev && !had) return
    if (had) {
      this.reindex(kind, id, had.view, undefined, batch.groups)
      this.rows.delete(k)
      batch.kinds.add(kind)
      batch.entities.add(k)
    }
    this.tomb.set(k, rev)
    batch.changed = true
  }

  private commit(batch: ChangeSet): ChangeSet {
    if (!batch.changed) return batch
    const nextVer = ++this.ver
    for (const kind of batch.kinds) this.kindVer.set(kind, nextVer)
    for (const group of batch.groups) {
      this.groupVer.set(group, nextVer)
      this.listCache.delete(group)
    }
    for (const k of batch.entities) this.entityVer.set(k, nextVer)
    const notify = new Set<() => void>()
    for (const fn of this.allListeners) notify.add(fn)
    for (const kind of batch.kinds) this.kindListeners.get(kind)?.forEach((fn) => notify.add(fn))
    for (const group of batch.groups) this.groupListeners.get(group)?.forEach((fn) => notify.add(fn))
    for (const k of batch.entities) this.entityListeners.get(k)?.forEach((fn) => notify.add(fn))
    notify.forEach((fn) => fn())
    return batch
  }

  /** A patch frame (`{type:"patch", ws, rev, ops}`). */
  mergePatch(rev: number, ops: PatchOp[]): ChangeSet {
    const batch = this.beginBatch()
    for (const op of ops) {
      if (op.op === "remove" || !op.view) this.remove(op.type, op.id, rev, batch)
      else this.upsert(op.type, op.id, op.view, rev, batch)
    }
    return this.commit(batch)
  }

  /** A snapshot: its views, and the hot views of its kinds it no longer holds go (they left the hot set or are gone). */
  mergeSnapshot(rev: number, views: SnapshotViews): ChangeSet {
    const batch = this.beginBatch()
    const kinds = Object.keys(views) as ViewKind[]
    const held = new Set<string>()
    for (const kind of kinds) {
      const rows = kind === "message" ? views.message?.views : views[kind]
      for (const v of (rows ?? []) as View[]) {
        held.add(key(kind, v.id))
        this.upsert(kind, v.id, v, rev, batch)
      }
    }
    for (const [k, row] of this.rows) {
      const kind = kindOf(k)
      if (kinds.includes(kind) && !held.has(k) && row.rev <= rev && HOT[kind](row.view)) {
        this.remove(kind, row.view.id, row.rev, batch)
      }
    }
    for (const kind of kinds) {
      if (kind !== "message" && rev > (this.floor[kind] ?? -1)) {
        this.floor[kind] = rev
        batch.changed = true
      }
    }
    return this.commit(batch)
  }

  /** Views read by id or in a page, as of `rev`; `missing`: ids that no longer exist. */
  mergeViews(rev: number, kind: ViewKind, views: View[], missing: string[] = []): ChangeSet {
    const batch = this.beginBatch()
    for (const v of views) this.upsert(kind, v.id, v, rev, batch)
    for (const id of missing) this.remove(kind, id, rev, batch)
    return this.commit(batch)
  }

  /** Reset all rows, indexes and tombstones (workspace switch or stream reset), notifying every subscriber once. */
  reset() {
    this.rows.clear()
    this.index.clear()
    this.tomb.clear()
    for (const k of Object.keys(this.floor) as ViewKind[]) delete this.floor[k]
    this.kindVer.clear()
    this.groupVer.clear()
    this.entityVer.clear()
    this.listCache.clear()
    this.resetVer = this.ver = this.ver + 1
    const notify = new Set<() => void>()
    for (const fn of this.allListeners) notify.add(fn)
    for (const set of this.kindListeners.values()) set.forEach((fn) => notify.add(fn))
    for (const set of this.groupListeners.values()) set.forEach((fn) => notify.add(fn))
    for (const set of this.entityListeners.values()) set.forEach((fn) => notify.add(fn))
    notify.forEach((fn) => fn())
  }

  viewOf<V extends View>(kind: ViewKind, id: string): V | undefined {
    return this.rows.get(key(kind, id))?.view as V | undefined
  }

  /** The views of a kind or group; keeps array identity until that group's version advances. */
  viewsOf<V extends View>(kind: ViewKind, group: string = kind): V[] {
    const ver = this.groupVersion(group)
    const cached = this.listCache.get(group)
    if (cached && cached.ver === ver) return cached.rows as V[]
    const ids = this.index.get(group)
    const rows: View[] = []
    if (ids) {
      for (const id of ids) {
        const row = this.rows.get(key(kind, id))
        if (row) rows.push(row.view)
      }
    }
    this.listCache.set(group, { ver, rows })
    return rows as V[]
  }
}
