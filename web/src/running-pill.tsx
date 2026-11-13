/* The running-status pill, on affectUI's Running Now, with Steve's own data: every open item's one state (item-state.ts
 * via itemDisplayState) sorted as the items page sorts it (item-lanes.ts), so the pill's "N 进行中 · M 等你" is the
 * page's. ui.tsx shows it in the sidebar and a phone's page header, so this imports nothing from ui.tsx. */
import { RunningNow, type RunningTask } from "@/components/affect/running-now"
import { itemDisplayState, workflowRelation, workflowRelationText } from "@/item-workflow-relation"
import { planProgress, resumable, RUN_ORDER, runStateOf, type RunState } from "@/item-lanes"
import { permissionQuestion } from "@/permission-question"
import { setSt, st, type Item } from "@/store"
import { useT } from "@/i18n"
import { useState } from "react"

/** what the item is on now: its open card's question, the workflow it waits on, or its own "now" line */
export function stepOf(i: Item, said: string): string {
  if (i.waits[0]) return permissionQuestion(i.waits[0].q)
  const relation = workflowRelation(i, st.items)
  if (relation) return workflowRelationText(relation)
  return i.now || said
}

/** the open items whose move is someone's: what needs you, what runs, what is paused */
export function runningTasks(t: ReturnType<typeof useT>): RunningTask[] {
  const rows: (RunningTask & { order: number })[] = []
  for (const i of st.items) {
    if (i.arch) continue
    const s = itemDisplayState(i), state: RunState | null = runStateOf(s)
    if (!state) continue
    const prog = planProgress(i.plan)
    // a plain pause says so before what it was on; a paused graph's line already says it is paused, and why
    const said = t(`views:itemState.${s.kind}`), on = stepOf(i, said)
    const step = s.kind === "paused" && on !== said ? `${said} · ${on}` : on
    rows.push({ id: i.id, title: `${i.id} ${i.t}`, step, done: prog?.done ?? 0, total: prog?.total ?? 0, state, resumable: resumable(s), order: RUN_ORDER[state] })
  }
  return rows.sort((a, b) => a.order - b.order)
}

/** `quiet`: nothing at all when no item is open (a phone's page header has no room to say so) */
export function RunningNowPill({ className, align = "start", quiet = false }: { className?: string; align?: "start" | "end"; quiet?: boolean }) {
  const t = useT()
  const [open, setOpen] = useState(false)
  const tasks = runningTasks(t)
  const leave = (h: string) => { setOpen(false); location.hash = h }
  if (quiet && !tasks.length) return null
  return (
    <RunningNow className={className} tasks={tasks} align={align} open={open} onOpenChange={setOpen}
      onPause={(id) => setSt(id, "pause")} onResume={(id) => setSt(id, "run")} onOpen={(id) => leave("#/items/" + id)} onOpenAll={() => leave("#/items")}
      labels={{ title: t("views:runningNow.title"), running: t("views:runningNow.running"), needsYou: t("views:runningNow.needsYou"), needsYouBadge: t("views:runningNow.needsYouBadge"),
        paused: t("views:runningNow.paused"), pausedCount: t("views:runningNow.pausedCount"), done: t("views:runningNow.done"), pause: t("views:runningNow.pause"), resume: t("views:runningNow.resume"),
        open: t("views:runningNow.open"), openAll: t("views:runningNow.openAll"), empty: t("views:runningNow.empty"), idle: t("views:runningNow.idle"), stepsOf: t("views:runningNow.stepsOf") }} />
  )
}
