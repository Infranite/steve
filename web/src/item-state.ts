/* ── an item's one state ──
 * One answer every surface reads: what is happening, and whose move it is. Pure, so it is tested alone. */
import type { ItemView } from "./gen/ItemView.ts"

export type StateKind =
  | "done" | "dropped" | "failed" | "proposed"
  | "asking"        // a card waits for you
  | "dispatching"   // Steve is choosing an executor Bot asynchronously
  | "paused"        // the item is paused
  | "blocked"       // waiting for its prerequisites
  | "working"       // a session is running
  | "queued"        // your message waits for a session to take it
  | "closing"       // the cycle is waiting to close
  | "idle"          // open, nothing running, nothing asked
export type StateSublabel = "you" | "waitingAgent"
export type Tone = "run" | "wait" | "pause" | "done" | "fail" | "neutral"
export type DisplayStatus = "prop" | "run" | "wait" | "pause" | "done" | "drop" | "block" | "fail"
export type ItemState = { kind: StateKind; tone: Tone; /** whose move: you, Steve, the agents, or nobody (closed) */ move: "you" | "steve" | "agents" | "none"; detail?: string }

/** the fields of a store `Item` this reads */
export type StateItem = { st: string; phase?: ItemView["state"]["kind"]; dispatching?: boolean; progress?: string; waits?: { q: string }[]; queued?: number; q?: unknown[] | null }

const TONE: Record<StateKind, Tone> = {
  done: "done", dropped: "neutral", failed: "fail", proposed: "wait", asking: "wait", dispatching: "run",
  paused: "pause", blocked: "pause", working: "run", queued: "run", closing: "run", idle: "neutral",
}
const YOURS: StateKind[] = ["proposed", "asking", "paused"]
const CLOSED: StateKind[] = ["done", "dropped", "failed"]
const CLOSED_SET: ReadonlySet<StateKind> = new Set(CLOSED)

/* the list's status of an item is its one state in lifecycle terms, so a row, its group and the item page agree */
const AS_ST: Record<StateKind, DisplayStatus> = {
  done: "done", dropped: "drop", failed: "fail", proposed: "prop", asking: "wait", dispatching: "run",
  paused: "pause", blocked: "block", working: "run", queued: "run", closing: "run", idle: "run",
}

/* optional sublabel beside the item page's state headline: shown only when it adds accurate, non-redundant guidance */
const SUBLABEL: Record<StateKind, StateSublabel | null> = {
  done: null,
  dropped: null,
  failed: null,
  proposed: null,
  asking: null,
  dispatching: null,
  paused: "you",
  blocked: null,
  working: null,
  queued: "waitingAgent",
  closing: null,
  idle: null,
}

const make = (kind: StateKind, detail?: string): ItemState =>
  ({ kind, tone: TONE[kind], move: CLOSED_SET.has(kind) ? "none" : YOURS.includes(kind) ? "you" : "agents", detail: detail || undefined })

/** `running`: one of the item's sessions is in a turn now */
export function itemState(i: StateItem, running: boolean): ItemState {
  if (i.st === "done") return make("done")
  if (i.st === "drop") return make("dropped")
  if (i.st === "fail") return make("failed", i.progress)
  if (i.st === "prop") return make("proposed")
  if (i.waits?.length) return make("asking", i.waits[0].q)
  if (i.phase === "closing") return { ...make("closing"), move: "steve" }
  if (i.dispatching) return { ...make("dispatching"), move: "steve" }
  if (running) return make("working", i.progress)
  if (i.st === "pause") return make("paused")
  if (i.st === "block") return make("blocked", i.progress)
  if (i.queued ?? i.q?.length) return make("queued")
  return make("idle", i.progress)
}

export function itemDisplayStatusOf(s: ItemState): DisplayStatus {
  return s.kind === "closing" && s.move === "you" ? "wait" : AS_ST[s.kind]
}

export function isClosedItemState(s: Pick<ItemState, "kind">): boolean {
  return CLOSED_SET.has(s.kind)
}

export function itemStateSublabel(s: Pick<ItemState, "kind">): StateSublabel | null {
  return SUBLABEL[s.kind]
}
