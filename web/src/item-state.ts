/* ── an item's one state ──
 * The item page used to say where an item stands in six places (lifecycle status, the root agent, the
 * collaboration header, the stage, the "now" line, the notices) and they disagreed. This is the one answer
 * they all read: what is happening, and whose move it is. Pure, so it is tested alone. */
import type { Workflow } from "./workflow.ts"

export type StateKind =
  | "done" | "dropped" | "failed" | "proposed"
  | "asking"        // a card waits for you
  | "replan"        // the work graph belongs to an earlier cycle: the coordinator has to plan again
  | "unavailable"   // the graph cannot execute (Node, principal, bot)
  | "graphPaused"   // the graph is paused
  | "paused"        // the item is paused
  | "blocked"       // waiting for its prerequisites
  | "working"       // a session is running
  | "queued"        // your message waits for a session to take it
  | "idle"          // open, nothing running, nothing asked
export type Tone = "run" | "wait" | "pause" | "done" | "fail" | "neutral"
export type ItemState = { kind: StateKind; tone: Tone; /** whose move: you, the agents, or nobody (closed) */ move: "you" | "agents" | "none"; detail?: string }

/** the fields of a store `Item` this reads */
export type StateItem = { st: string; now?: string; waits?: { q: string }[]; q?: unknown[] | null }

const TONE: Record<StateKind, Tone> = {
  done: "done", dropped: "neutral", failed: "fail", proposed: "wait", asking: "wait", replan: "wait", unavailable: "fail",
  graphPaused: "pause", paused: "pause", blocked: "pause", working: "run", queued: "run", idle: "neutral",
}
const YOURS: StateKind[] = ["proposed", "asking", "replan", "unavailable", "graphPaused", "paused"]
const CLOSED: StateKind[] = ["done", "dropped", "failed"]

const make = (kind: StateKind, detail?: string): ItemState =>
  ({ kind, tone: TONE[kind], move: CLOSED.includes(kind) ? "none" : YOURS.includes(kind) ? "you" : "agents", detail: detail || undefined })

/** `running`: one of the item's sessions is in a turn now */
export function itemState(i: StateItem, workflow: Workflow | null | undefined, running: boolean): ItemState {
  if (i.st === "done") return make("done")
  if (i.st === "drop") return make("dropped")
  if (i.st === "fail") return make("failed", i.now)
  if (i.st === "prop") return make("proposed")
  if (i.waits?.length) return make("asking", i.waits[0].q)
  if (running) return make("working", i.now)
  if (workflow && workflow.acceptance_mode !== "single_session") {
    if (workflow.current_cycle === false) return make("replan")
    if (workflow.execution_ready === false) return make("unavailable", workflow.blockers[0])
    if (workflow.paused) return make("graphPaused", workflow.blockers[0])
  }
  if (i.st === "pause") return make("paused")
  if (i.st === "block") return make("blocked", i.now)
  if (i.q?.length) return make("queued")
  return make("idle", i.now)
}
