import type { Item } from "@/store"
import { t } from "@/i18n"

const oldWorkflow = (item: Item): boolean => item.workflowCycle !== undefined && item.cycle !== undefined && item.workflowCycle !== item.cycle

export const itemDisplayStatus = (item: Item): Item["st"] => item.st === "run" && item.workflowPaused && !oldWorkflow(item) ? "pause" : item.st

export type WorkflowRelation = { kind: "parent"; children: string[] } | { kind: "child"; parent: string } | { kind: "paused"; failed: string[] } | { kind: "previous" }

export function workflowRelationText(relation: WorkflowRelation): string {
  if (relation.kind === "child") return t("shell:item.childOf", { id: relation.parent })
  if (relation.kind === "paused") return relation.failed.length
    ? t("shell:item.pausedFailed", { ids: relation.failed.join(t("shell:item.relationSep")) })
    : t("shell:item.pausedWorkflow")
  if (relation.kind === "previous") return t("shell:item.previousWorkflow")
  return t("shell:item.waitingFor", { ids: relation.children.join(t("shell:item.relationSep")) })
}

/** A workflow child can be shown in a different list position (or hidden by a filter).
 * Use the workspace's full item set, not the visible rows, to explain who is waiting on it. */
export function workflowRelation(item: Item, items: Item[]): WorkflowRelation | null {
  if (item.src[1] === "workflow" && item.parent?.item) return { kind: "child", parent: item.parent.item }
  if (item.st !== "run" || item.phase !== "active") return null
  if (oldWorkflow(item)) return { kind: "previous" }
  if (item.workflowPaused) return { kind: "paused", failed: item.workflowFailed ?? [] }
  const children = items.filter((other) => other.src[1] === "workflow" && other.parent?.item === item.id && !other.arch && (other.phase === "ready" || other.phase === "active" || other.phase === "closing")).map((other) => other.id)
  return children.length ? { kind: "parent", children } : null
}
