/* ST-50: history pages and the latest messages share one revisioned view table. */
import { useSyncExternalStore } from "react"
import { modReq, qs, type ConversationMessage } from "@/api"
import { chatMessage, closedSt, msgOf, msgOfView, type Msg } from "@/store"
import { itemView, loadMessagePage, messagePageState, messageViews, onViews, scopeKey, table, useMessagePage } from "@/data/sync"

const empty: string[] = []
let ids = empty, lastTable: ReturnType<typeof table> | undefined
function threadIds() {
  const t = table()
  if (t === lastTable) return ids
  lastTable = t
  const next = messageViews().filter((v) => {
    const m = msgOfView(v), meta = v.meta as { hidden?: boolean } | null
    return !m.held && !meta?.hidden && !m.notice?.closed && !(m.notice && m.item && closedSt(itemView(m.item)?.status))
  }).sort((a, b) => a.created - b.created || (a.id < b.id ? -1 : a.id === b.id ? 0 : 1)).map((v) => v.id)
  if (next.length !== ids.length || next.some((id, i) => id !== ids[i])) ids = next
  return ids
}

export const useThreadIds = (enabled = true) => useSyncExternalStore(onViews, enabled ? threadIds : () => empty)
export const threadMsg = chatMessage
export const useThreadMessage = (id?: string) => useSyncExternalStore(onViews, () => id ? chatMessage(id) : undefined)
export const threadMessages = () => threadIds().map((id) => chatMessage(id)!)
export const historyState = () => {
  const p = messagePageState()
  return { more: !!p.next, ended: !p.next, loading: p.pending }
}
export function useHistory() { useMessagePage(); return historyState() }
export const loadOlder = () => loadMessagePage().catch(() => false)

/** Load the preceding pages until we can scroll directly to the requested message. */
export async function fillTo(created: number, signal?: AbortSignal): Promise<void> {
  const scope = scopeKey()
  while (scope === scopeKey() && !signal?.aborted) {
    const first = threadMsg(threadIds()[0])
    if ((first?.created != null && first.created < created) || !messagePageState().next) break
    if (!await loadMessagePage(signal).catch(() => false)) break
  }
}

export async function searchThread(q: string, signal?: AbortSignal): Promise<{ msgs: Msg[]; local: boolean }> {
  const x = q.trim()
  if (!x) return { msgs: [], local: false }
  try {
    const r = await modReq<{ messages: ConversationMessage[] }>("GET", "/thread/search" + qs({ q: x }), undefined, signal)
    return { msgs: r.messages.map(msgOf).filter((m) => !m.held && (m.k === "me" || m.k === "steve")), local: false }
  } catch (e) {
    if (signal?.aborted) throw e
    const low = x.toLowerCase()
    const msgs = threadMessages().filter((m) => (m.t ?? "").toLowerCase().includes(low) || m.item?.toLowerCase() === low).reverse()
    return { msgs, local: true }
  }
}

/* ───────── jumping to a message, and back ───────── */
export type Jump = { id: string; created?: number }
const JUMP = "steve-chat-jump"
/** a jump no conversation took yet (the reader was elsewhere and the chat view is still on its way), kept a moment
 *  for the one that mounts next */
let pending: { j: Jump; at: number } | null = null
const KEEP = 2000
/** go to a message of the conversation (a search hit, a topic, a quote), from wherever the reader is: right away when
 *  a conversation is shown, or as soon as one is (within a couple of seconds) */
export function jumpToMsg(id: string, created?: number) { pending = { j: { id, created }, at: Date.now() }; window.dispatchEvent(new CustomEvent<Jump>(JUMP, { detail: { id, created } })) }
export function onJump(f: (j: Jump) => void) {
  const h = (e: Event) => { pending = null; f((e as CustomEvent<Jump>).detail) }
  window.addEventListener(JUMP, h)
  /* the jump that waited for a conversation, once this one is listening */
  let live = true
  queueMicrotask(() => { if (live && pending && Date.now() - pending.at < KEEP) { const { j } = pending; pending = null; f(j) } })
  return () => { live = false; window.removeEventListener(JUMP, h) }
}
