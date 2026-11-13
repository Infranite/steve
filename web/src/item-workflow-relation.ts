import { st, type Item } from "@/store"
import { itemState, type StateKind } from "@/item-state"
import type { Workflow } from "@/workflow"
import { t } from "@/i18n"

const oldWorkflow = (item: Item): boolean => item.workflowCycle !== undefined && item.cycle !== undefined && item.workflowCycle !== item.cycle

/* the list's status of an item is its one state (item-state.ts) in the lifecycle's terms, so a row, its group and
 * the item page agree: an item whose move is yours is under 等你, a paused graph under 已暂停. The list has only the
 * graph's pause flag and cycle; the page's loaded graph adds the rest. */
const AS_ST: Record<StateKind, Item["st"]> = {
  done: "done", dropped: "drop", failed: "fail", proposed: "prop", asking: "wait", unavailable: "wait",
  graphPaused: "pause", paused: "pause", blocked: "block", working: "run", queued: "run", idle: "run",
}
const graphOf = (item: Item): Workflow | null => item.workflow ?? (item.workflowCycle === undefined && !item.workflowPaused ? null
  : { item_id: item.id, revision: 0, paused: !!item.workflowPaused, max_parallel: 1, max_attempts: 1, units: [], blockers: [], history: [], current_cycle: !oldWorkflow(item) })
export const itemDisplayState = (item: Item) => itemState(item, graphOf(item), (item.ss ?? []).some((k) => st.S?.[k]?.st === "run"))
export const itemDisplayStatus = (item: Item): Item["st"] => AS_ST[itemDisplayState(item).kind]

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
