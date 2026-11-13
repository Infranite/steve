/* ── where an item goes on the board, in the items page's counts and filters, and in the running-status pill ──
 * All of them read an item's one state (item-state.ts) and sort it the way the items list groups it
 * (item-workflow-relation.ts AS_ST), so the board, the page's "N 件进行中 · M 件等你", its 等你 / 进行中 filters and the
 * pill all mean the same thing:
 *   needs    a card or a proposal waits for your answer, or the graph cannot execute until you fix it (the list's 等你)
 *   running  a session works, your message is queued, or the item is open with nothing asked (the list's 进行中)
 *   paused   the item or its graph is paused, or it waits for its prerequisites (the list's 已暂停 and 等前置)
 *   done     finished
 * Dropped and failed items are closed without a result to show, so none of them lists those. Pure, so it is tested
 * alone. */
import type { ItemState, StateKind } from "./item-state.ts"

export type Lane = "needs" | "running" | "paused" | "done"
/** the pill's states: affectUI Running Now's running / needs-you / paused */
export type RunState = "running" | "needs-you" | "paused"

const LANE: Record<StateKind, Lane | null> = {
  proposed: "needs", asking: "needs", unavailable: "needs",
  working: "running", queued: "running", idle: "running",
  paused: "paused", graphPaused: "paused", blocked: "paused",
  done: "done", dropped: null, failed: null,
}
export const laneOf = (s: ItemState): Lane | null => LANE[s.kind]

/** null: not in the pill (closed or done) */
export function runStateOf(s: ItemState): RunState | null {
  const lane = laneOf(s)
  return lane === "needs" ? "needs-you" : lane === "running" ? "running" : lane === "paused" ? "paused" : null
}

/** a paused item resumes from the pill; a paused graph resumes on its own page, where its failures are; an item waiting
 *  for its prerequisites goes on when they are done */
export const resumable = (s: ItemState) => s.kind === "paused"

/** plan steps done and in all; null without a plan */
export function planProgress(plan: [string, number][] | undefined): { done: number; total: number } | null {
  if (!plan?.length) return null
  return { done: plan.filter((x) => x[1] === 1).length, total: plan.length }
}

/** the pill lists what needs you first, then what runs, then what is paused */
export const RUN_ORDER: Record<RunState, number> = { "needs-you": 0, running: 1, paused: 2 }

/** a line that starts with the state it is shown beside says only the rest ("协作已暂停 · ST-41 失败" beside
 *  "协作已暂停" is "ST-41 失败"); a line that is only the state again says nothing */
export function besideState(line: string, said: string): string {
  if (!line || !said || !line.startsWith(said)) return line
  return line.slice(said.length).replace(/^[\s·:\uFF1A,\uFF0C;\uFF1B–-]+/, "") // the separators, ASCII and full-width
}
