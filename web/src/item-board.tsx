import { getItems } from "@/store"
/* The items board, on affectUI's Agent Inbox, with Steve's own data: every item's one state (item-state.ts via
 * itemDisplayState) decides its column the way the items page counts and filters it (item-lanes.ts).
 * Plan counts are projected with the list. The plan text is loaded when the item is opened. */
import { AgentInbox, type AgentTask } from "@/components/affect/agent-inbox"
import { isWorkflowChild, itemDisplayState, partitionSubtasks } from "@/item-workflow-relation"
import { besideState, laneOf } from "@/item-lanes"
import { stepOf } from "@/running-pill"
import { botName, type Item } from "@/store"
import { go, Meta, Tags } from "@/ui"
import { dueLabel, ProjectChip } from "@/view-shared"
import { useT } from "@/i18n"

/** `cap`: the Done column shows this many until you ask for all of them, as the list folds its closed groups */
export function ItemsBoard({ xs, cap }: { xs: Item[]; cap: number }) {
  const t = useT()
  const pool = getItems().filter((x) => !x.arch)
  const tasks: AgentTask[] = xs.flatMap((i) => {
    const s = itemDisplayState(i), lane = laneOf(s)
    if (!lane) return []
    const kids = pool.filter((c) => isWorkflowChild(c) && c.parent?.item === i.id)
    const sub = kids.length ? partitionSubtasks(i, kids) : null
    const said = t(`views:itemState.${s.kind}`)
    // the line under the title: what it is on, without the state the pill beside it already says
    const desc = besideState(stepOf(i, ""), said)
    const details = (i.project || i.due || i.tags?.length || sub) ? (
      <div className="flex flex-col gap-1.5">
        {sub && <span className="type-footnote text-muted-foreground" data-board-subtasks={i.id}>{sub.live.length > 0 ? t("views:items.subtasksLive", { live: sub.live.length, done: sub.doneCount }) : t("views:items.subtasksDone", { count: sub.doneCount || sub.total })}</span>}
        {(i.project || i.due) && <Meta>{i.project && <ProjectChip id={i.project} plain />}{i.due && <span>{t("views:items.due", { due: dueLabel(i.due) })}</span>}</Meta>}
        {!!i.tags?.length && <Tags ids={i.tags} />}
      </div>
    ) : undefined
    return [{
      id: i.id, code: i.id, title: i.t, description: desc && desc !== said ? desc : undefined, agent: botName(i.b), lane,
      status: lane === "done" ? "done" : s.tone === "fail" ? "failed" : lane === "needs" ? "waiting" : "running", stateText: said, tone: s.tone, time: i.up,
      progress: i.planTotal ? { done: i.planDone ?? 0, total: i.planTotal } : undefined, details,
    } satisfies AgentTask]
  })
  return (
    <div data-items-board>
      <AgentInbox tasks={tasks} bar={false} view="board" cap={{ done: cap }} onOpen={(task) => go("#/items/" + task.id)}
        labels={{ needs: t("views:board.needs"), running: t("views:board.running"), paused: t("views:board.paused"), done: t("views:board.done"), waitingState: t("views:board.waiting"), runningState: t("views:board.runningState"),
          doneState: t("views:board.doneState"), failedState: t("views:board.failed"), doneIn: t("views:board.doneIn"), empty: t("views:board.empty"), open: t("views:board.open"),
          showAll: t("views:board.showAll"), collapse: t("views:board.collapse"), summary: t("views:board.summary"), progress: t("views:board.progress") }} />
    </div>
  )
}
