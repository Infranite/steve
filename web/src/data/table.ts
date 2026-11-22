/* The views table: every view this page holds (`steve_types::view` on the Hub), from the snapshot, patch frames,
 * reads by id and pages, merged by one rule: per (kind, id) the copy with the larger `rev` wins, a later one on a tie.
 * Pure functions on an immutable table, so a merge that changes nothing returns the same table and nobody re-renders;
 * a view that did not change keeps its object.
 * Relative, type-only imports so node --test loads this file without a bundler. */
import type { DecisionView } from "../gen/DecisionView.ts"
import type { PatchOp } from "../gen/PatchOp.ts"
import type { SnapshotViews } from "../gen/SnapshotViews.ts"
import type { View } from "../gen/View.ts"
import type { ViewKind } from "../gen/ViewKind.ts"

export type Row = { view: View; rev: number }
export type Table = {
  rows: ReadonlyMap<string, Row>
  /** removed entities: their key and the rev they were removed at */
  tomb: ReadonlyMap<string, number>
  /** per kind, the rev of the last snapshot: a hot view at or before it that the snapshot did not hold is stale */
  floor: Readonly<Partial<Record<ViewKind, number>>>
  /** the newest rev merged */
  rev: number
}

export const EMPTY: Table = { rows: new Map(), tomb: new Map(), floor: {}, rev: 0 }

/** The Hub's hot sets (steve-hub `projection/kinds.rs` `is_hot`): what a snapshot holds of each kind. */
export const HOT: Record<ViewKind, (v: View) => boolean> = {
  decision: (v) => (v as DecisionView).status === "open",
}

const key = (kind: ViewKind, id: string) => `${kind}:${id}`
const kindOf = (k: string) => k.slice(0, k.indexOf(":")) as ViewKind

/** A table being changed: maps are copied on the first change only. */
class Draft {
  rows: Map<string, Row> | null = null
  tomb: Map<string, number> | null = null
  floor: Table["floor"]
  rev: number
  readonly t: Table
  constructor(t: Table, rev: number) { this.t = t; this.floor = t.floor; this.rev = Math.max(t.rev, rev) }
  get r(): ReadonlyMap<string, Row> { return this.rows ?? this.t.rows }
  get tm(): ReadonlyMap<string, number> { return this.tomb ?? this.t.tomb }
  w() { return (this.rows ??= new Map(this.t.rows)) }
  wt() { return (this.tomb ??= new Map(this.t.tomb)) }

  upsert(kind: ViewKind, id: string, view: View, rev: number) {
    const k = key(kind, id), had = this.r.get(k)
    if (had && had.rev > rev) return
    if ((this.tm.get(k) ?? -1) > rev) return
    if (!had && rev <= (this.floor[kind] ?? -1) && HOT[kind](view)) return
    const same = had && JSON.stringify(had.view) === JSON.stringify(view)
    if (same && had.rev === rev) return
    this.w().set(k, { view: same ? had.view : view, rev })
    if (this.tm.has(k)) this.wt().delete(k)
  }

  remove(kind: ViewKind, id: string, rev: number) {
    const k = key(kind, id), had = this.r.get(k)
    if (had && had.rev > rev) return
    if ((this.tm.get(k) ?? -1) >= rev && !had) return
    if (had) this.w().delete(k)
    this.wt().set(k, rev)
  }

  done(): Table {
    if (!this.rows && !this.tomb && this.floor === this.t.floor) return this.t
    return { rows: this.r, tomb: this.tm, floor: this.floor, rev: this.rev }
  }
}

/** A patch frame (`{type:"patch", ws, rev, ops}`). */
export function mergePatch(t: Table, rev: number, ops: PatchOp[]): Table {
  const d = new Draft(t, rev)
  for (const op of ops) {
    if (op.op === "remove" || !op.view) d.remove(op.type, op.id, rev)
    else d.upsert(op.type, op.id, op.view, rev)
  }
  return d.done()
}

/** A snapshot: its views, and the hot views of its kinds it no longer holds go (they left the hot set or are gone). */
export function mergeSnapshot(t: Table, rev: number, views: SnapshotViews): Table {
  const d = new Draft(t, rev)
  const kinds = Object.keys(views) as ViewKind[]
  const held = new Set<string>()
  for (const kind of kinds) {
    for (const v of (views[kind] ?? []) as View[]) { held.add(key(kind, v.id)); d.upsert(kind, v.id, v, rev) }
  }
  for (const [k, row] of d.r) {
    const kind = kindOf(k)
    if (kinds.includes(kind) && !held.has(k) && row.rev <= rev && HOT[kind](row.view)) d.remove(kind, row.view.id, rev)
  }
  d.floor = { ...d.floor, ...Object.fromEntries(kinds.map((k) => [k, Math.max(rev, d.floor[k] ?? 0)])) }
  return d.done()
}

/** Views read by id or in a page, as of `rev`; `missing`: ids that no longer exist. */
export function mergeViews(t: Table, rev: number, kind: ViewKind, views: View[], missing: string[] = []): Table {
  const d = new Draft(t, rev)
  for (const v of views) d.upsert(kind, v.id, v, rev)
  for (const id of missing) d.remove(kind, id, rev)
  return d.done()
}

export const viewOf = <V extends View>(t: Table, kind: ViewKind, id: string): V | undefined => t.rows.get(key(kind, id))?.view as V | undefined

/** The views of a kind, in the table's order; the same array while the table is the same. */
const lists = new WeakMap<Table, Map<ViewKind, View[]>>()
export function viewsOf<V extends View>(t: Table, kind: ViewKind): V[] {
  let m = lists.get(t)
  if (!m) lists.set(t, (m = new Map()))
  let xs = m.get(kind)
  if (!xs) { xs = []; for (const [k, row] of t.rows) if (kindOf(k) === kind) xs.push(row.view); m.set(kind, xs) }
  return xs as V[]
}
