/* A channel conversation's full record (#243) woven into its conversation: the turns Steve was handed stay as they are,
 * and every other message of the chat comes in between them by time. A received message that has a turn is shown by that
 * turn (its handling goes under the sender line); one Steve said through a turn is shown by Steve's message (ChanOutNote
 * says what became of it). Pure, so it is tested without a page. */
import type { ConvRecord, HandlingState } from "@/api/conversations"

/** what the weave needs of a conversation message */
type MsgLike = { id?: string; created?: number }
export type Entry<M extends MsgLike> = { k: "msg"; m: M } | { k: "rec"; r: ConvRecord }

/** `entries`: messages and the records not shown by one of them, oldest first; `byTurn`: a turn's id → the received
 *  message it stands for */
export function weave<M extends MsgLike>(msgs: readonly M[], records: readonly ConvRecord[]): { entries: Entry<M>[]; byTurn: Map<string, ConvRecord> } {
  const ids = new Set(msgs.map((m) => m.id).filter(Boolean))
  const byTurn = new Map<string, ConvRecord>()
  const rest: ConvRecord[] = []
  for (const r of records) {
    if (r.turn && ids.has(r.turn)) { if (r.direction === "in") byTurn.set(r.turn, r) }
    else rest.push(r)
  }
  rest.sort((a, b) => a.created - b.created || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
  const entries: Entry<M>[] = []
  let n = 0
  for (const m of msgs) {
    /* a message still being written has no time yet: it stays where the conversation put it */
    if (m.created !== undefined) while (n < rest.length && rest[n].created <= m.created) entries.push({ k: "rec", r: rest[n++] })
    entries.push({ k: "msg", m })
  }
  while (n < rest.length) entries.push({ k: "rec", r: rest[n++] })
  return { entries, byTurn }
}

/** the newest time the record and the conversation show: how far you have read once it is on screen */
export function shownUpTo(msgs: readonly MsgLike[], records: readonly ConvRecord[]): number {
  let at = 0
  for (const m of msgs) if (m.created && m.created > at) at = m.created
  for (const r of records) if (r.created > at) at = r.created
  return at
}

/** how a handling reads: its tone, and whether it is worth a line at all (nothing known says nothing) */
export const HANDLING_TONE: Record<HandlingState, "success" | "warning" | "danger" | "info" | "neutral" | null> = {
  answered: "success", drafted: "warning", blocked: "danger", unknown: "warning", recalled: "neutral", local: "neutral", waiting: "warning", item: "info", working: "info", failed: "danger", skipped: "neutral", decided: "neutral", recorded: "neutral", none: null,
}

/** the detail after a handling's name: the failure's reason, else what the event was recorded with */
export function handlingDetail(h: { state: HandlingState; label: string; reason: string; error: string | null }): string {
  if (h.state === "failed" || h.state === "blocked" || h.state === "unknown") return h.error || h.reason || h.label
  if (h.state === "skipped" || h.state === "recorded" || h.state === "decided") return [h.label, h.reason].filter(Boolean).join(" · ")
  return ""
}
