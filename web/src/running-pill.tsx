import { itemViews, openDecisions, sessionViews, table } from "@/data/sync"
import type { ItemView } from "@/gen"
import { actOnItem, getItems, itemDisplayState, st, useStoreSelector, type Item } from "@/store"
/* The running-status pill, on affectUI's Running Now, with Steve's own data: every open item's one state (item-state.ts
 * via itemDisplayState) sorted as the items page sorts it (item-lanes.ts), so the pill's "N 进行中 · M 等你" is the
 * page's. ui.tsx shows it in the sidebar and a phone's page header, so this imports nothing from ui.tsx. */
import { RunningNow, type RunningTask } from "@/components/affect/running-now"
import { resumable, RUN_ORDER, runStateOf, type RunState } from "@/item-lanes"
import { permissionQuestion } from "@/permission-question"
import { useT } from "@/i18n"
import { useMemo, useState } from "react"

/** what the item is on now: its open card's question or its own "now" line */
export function stepOf(i: Item, said: string): string {
  if (i.waits[0]) return permissionQuestion(i.waits[0].q)
  return i.progress || said
}

/** the open items whose move is someone's: what needs you, what runs, what is paused */
export function runningTasks(t: ReturnType<typeof useT>): RunningTask[] {
  const rows: (RunningTask & { order: number })[] = []
  for (const i of getItems()) {
    if (i.arch) continue
    const s = itemDisplayState(i), state: RunState | null = runStateOf(s)
    if (!state) continue
    const said = t(`views:itemState.${s.kind}`), on = stepOf(i, said)
    const step = s.kind === "paused" && on !== said ? `${said} · ${on}` : on
    rows.push({ id: i.id, title: `${i.id} ${i.t}`, step, done: i.planDone ?? 0, total: i.planTotal ?? 0, state, resumable: resumable(s), order: RUN_ORDER[state] })
  }
  return rows.sort((a, b) => a.order - b.order)
}

let inputs: { table: ReturnType<typeof table>; key: string } | undefined
const itemStatusKeys = new WeakMap<ItemView, string>()
function itemStatusKey(item: ItemView) {
  let key = itemStatusKeys.get(item)
  if (key === undefined) {
    const { star, updated, detail_stamp, ...status } = item
    key = JSON.stringify(status)
    itemStatusKeys.set(item, key)
  }
  return key
}

function workSummary() {
  const current = table()
  if (inputs?.table === current) return inputs.key
  const key = JSON.stringify([
    itemViews().map(itemStatusKey),
    sessionViews().map(({ id, item_id, status, busy, waiting, queued }) => [id, item_id, status, busy, waiting, queued]),
    openDecisions(),
  ])
  inputs = { table: current, key }
  return key
}

export const useWorkSummary = () => useStoreSelector(workSummary)

/** `quiet`: nothing at all when no item is open (a phone's page header has no room to say so) */
export function RunningNowPill({ className, align = "start", quiet = false }: { className?: string; align?: "start" | "end"; quiet?: boolean }) {
  const t = useT()
  const [open, setOpen] = useState(false)
  const work = useWorkSummary()
  const bots = useStoreSelector(() => st.bots)
  const tasks = useMemo(() => runningTasks(t), [work, bots, t])
  const leave = (h: string) => { setOpen(false); location.hash = h }
  if (quiet && !tasks.length) return null
  return (
    <RunningNow className={className} tasks={tasks} align={align} open={open} onOpenChange={setOpen}
      onPause={(id) => actOnItem(id, "pause")} onResume={(id) => actOnItem(id, "resume")} onOpen={(id) => leave("#/items/" + id)} onOpenAll={() => leave("#/items")}
      labels={{ title: t("views:runningNow.title"), running: t("views:runningNow.running"), needsYou: t("views:runningNow.needsYou"), needsYouBadge: t("views:runningNow.needsYouBadge"),
        paused: t("views:runningNow.paused"), pausedCount: t("views:runningNow.pausedCount"), done: t("views:runningNow.done"), pause: t("views:runningNow.pause"), resume: t("views:runningNow.resume"),
        open: t("views:runningNow.open"), openAll: t("views:runningNow.openAll"), empty: t("views:runningNow.empty"), idle: t("views:runningNow.idle"), stepsOf: t("views:runningNow.stepsOf") }} />
  )
}
