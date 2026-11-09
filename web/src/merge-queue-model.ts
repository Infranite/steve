/* The merge queue's page (S8), the parts with no React in them: which policy fields there are and how
 * each is edited (counts, seconds shown as minutes, an optional number, a list of commands, a switch),
 * an input read back into a policy patch, and the batches ordered as a bisection runs them. */
import type { RQueueBatch, RQueuePolicy } from "@/api"

export type FieldKind = "count" | "minutes" | "optionalMinutes" | "list" | "switch"
export type Field = { key: keyof RQueuePolicy; kind: FieldKind }

/* every number of the policy, in the order the page shows them (merging, checks, releasing, the Node) */
export const POLICY_FIELDS: Field[] = [
  { key: "max_batch", kind: "count" },
  { key: "settle_s", kind: "minutes" },
  { key: "checks", kind: "list" },
  { key: "check_deadline_s", kind: "minutes" },
  { key: "host_deadline_s", kind: "minutes" },
  { key: "host_poll_s", kind: "minutes" },
  { key: "moved_retries", kind: "count" },
  { key: "release_window_s", kind: "minutes" },
  { key: "release_max", kind: "count" },
  { key: "release_idle_s", kind: "optionalMinutes" },
  { key: "cut_ci_wait_s", kind: "minutes" },
  { key: "cut_deadline_s", kind: "minutes" },
  { key: "yolo_reruns", kind: "count" },
  { key: "node_wait_s", kind: "minutes" },
  { key: "paused", kind: "switch" },
]

/* what an input shows for a field's value (seconds as minutes, a list one per line) */
export function shown(f: Field, v: unknown): string {
  if (v == null) return ""
  switch (f.kind) {
    case "minutes": case "optionalMinutes": return String(Math.round(((v as number) / 60) * 100) / 100)
    case "list": return (v as string[]).join("\n")
    case "switch": return v ? "1" : ""
    default: return String(v)
  }
}

/* an input read back: `undefined` when it does not read as the field's kind; `null` (blank) is back to
 * the default */
export function read(f: Field, text: string): unknown {
  const s = text.trim()
  if (f.kind === "list") return s === "" ? null : s.split("\n").map((l) => l.trim()).filter(Boolean)
  if (f.kind === "switch") return s !== ""
  if (s === "") return null
  const n = Number(s)
  if (!Number.isFinite(n) || n < 0) return undefined
  if (f.kind === "count") return Number.isInteger(n) ? n : undefined
  return Math.round(n * 60)
}

/* the fields whose input differs from what is set now, as a policy patch; `undefined` when one does not
 * read */
export function patchOf(set: Partial<RQueuePolicy>, inputs: Partial<Record<keyof RQueuePolicy, string>>): Partial<Record<keyof RQueuePolicy, unknown>> | undefined {
  const out: Partial<Record<keyof RQueuePolicy, unknown>> = {}
  for (const f of POLICY_FIELDS) {
    const text = inputs[f.key]
    if (text === undefined) continue
    const v = read(f, text)
    if (v === undefined) return undefined
    const now = set[f.key] ?? null
    if (JSON.stringify(v) !== JSON.stringify(now)) out[f.key] = v
  }
  return out
}

/* the batches as the page lists them: the live ones (under way, waiting their turn, asking) first, a
 * bisection's halves under the batch they split from, then the ended ones, newest first */
export function ordered(bs: RQueueBatch[]): { b: RQueueBatch; depth: number }[] {
  const live = (b: RQueueBatch) => ["running", "waiting", "unknown"].includes(b.state)
  const byNo = new Map(bs.map((b) => [b.no, b]))
  const depth = (b: RQueueBatch) => { let d = 0, p = b.parent; while (p != null && byNo.has(p) && d < 32) { d++; p = byNo.get(p)!.parent } return d }
  const roots = [...new Set(bs.map((b) => b.root))].sort((a, b) => {
    const la = bs.some((x) => x.root === a && live(x)), lb = bs.some((x) => x.root === b && live(x))
    return la === lb ? b - a : la ? -1 : 1
  })
  const out: { b: RQueueBatch; depth: number }[] = []
  for (const r of roots) {
    const tree = bs.filter((b) => b.root === r).sort((a, b) => a.no - b.no)
    const walk = (parent: number | null) => {
      for (const b of tree.filter((x) => x.parent === parent)) { out.push({ b, depth: depth(b) }); walk(b.no) }
    }
    walk(null)
    for (const b of tree) if (!out.some((o) => o.b.no === b.no)) out.push({ b, depth: depth(b) })
  }
  return out
}
