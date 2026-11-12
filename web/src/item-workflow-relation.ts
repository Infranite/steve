import type { Item } from "@/store"

/** A workflow child can be shown in a different list position (or hidden by a filter).
 * Use the workspace's full item set, not the visible rows, to explain who is waiting on it. */
export function workflowRelation(item: Item, items: Item[]): { kind: "parent"; children: string[] } | { kind: "child"; parent: string } | null {
  if (item.src[1] === "workflow" && item.parent?.item) return { kind: "child", parent: item.parent.item }
  if (item.st !== "run" || item.phase !== "active") return null
  const children = items.filter((other) => other.src[1] === "workflow" && other.parent?.item === item.id && !other.arch && (other.phase === "ready" || other.phase === "active" || other.phase === "closing")).map((other) => other.id)
  return children.length ? { kind: "parent", children } : null
}
