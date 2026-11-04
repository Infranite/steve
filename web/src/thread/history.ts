/* ST-50: the main conversation is one line however long it gets. /api/state carries its latest 200 messages; what was
 * said before them is loaded here on demand (scrolling to the top, a search hit, a restored reading position) and kept
 * for the life of the page, so the window moving on as new messages come does not tear a gap into what you read. */
import { useSyncExternalStore } from "react"
import { modReq, qs, type RMessage } from "@/api"
import { knownMsgs as known, msgOf, st, type Msg } from "@/store"

let more = true, ended = false, loading = false, ver = 0, scope = ""
const ls = new Set<() => void>()
const bump = () => { ver++; ls.forEach((f) => f()) }
export function useHistory() { return useSyncExternalStore((cb) => { ls.add(cb); return () => ls.delete(cb) }, () => ver) }
/** `ended`: the Hub said this is where the conversation starts (a Hub without the history route cannot say) */
export const historyState = () => ({ more, ended, loading })

/* another workspace is another conversation */
function sameScope() { const ws = st.workspace?.id ?? ""; if (ws !== scope) { scope = ws; known.clear(); more = true; ended = false } }

/** the conversation as shown: what was loaded before the window, then the window (which wins for what it holds) */
export function mainThread(thread: Msg[]): Msg[] {
  sameScope()
  for (const m of thread) if (m.id && m.created) known.set(m.id, m)
  const start = thread.find((m) => m.created)?.created
  if (start == null) return thread
  const older = [...known.values()].filter((m) => m.created! < start).sort((a, b) => a.created! - b.created!)
  return older.length ? [...older, ...thread] : thread
}
export const threadMsg = (id: string) => st.thread.find((m) => m.id === id) ?? known.get(id)
const oldest = () => { let n = Infinity; for (const m of known.values()) n = Math.min(n, m.created!); for (const m of st.thread) if (m.created) n = Math.min(n, m.created); return n }

async function page(before: number, after?: number, limit = after ? 2000 : 100) {
  const r = await modReq<{ messages: RMessage[]; has_more: boolean }>("GET", "/thread/history" + qs({ before: Number.isFinite(before) ? before : undefined, after, limit }))
  /* as the window shows them: a failure line of a finished item is history the notifications keep */
  const over = (id: string | null) => { const st0 = id ? st.items.find((i) => i.id === id)?.st : undefined; return st0 === "done" || st0 === "drop" }
  for (const m of r.messages) if (!m.meta?.held && !(m.meta?.notice && over(m.item_id))) known.set(m.id, msgOf(m))
  if (!after) ended = !r.has_more
  return r.has_more
}
/** the 100 messages before the oldest one shown; false when there is nothing more (or this Hub cannot say) */
export async function loadOlder(): Promise<boolean> {
  if (loading || !more) return false
  sameScope(); loading = true; bump()
  try { more = await page(oldest()); return true } catch { more = false; return false } finally { loading = false; bump() }
}
/** everything from the message created at `created` up to what is shown, and a little before it, so a jump lands in an
 *  unbroken conversation with the lead-up in view */
export async function fillTo(created: number): Promise<void> {
  sameScope()
  const from = oldest()
  if (!(created < from)) return
  loading = true; bump()
  try { if (await page(from, created)) more = await page(created, undefined, CONTEXT); else more = false } catch { /* the jump falls back to what is loaded */ } finally { loading = false; bump() }
}
const CONTEXT = 20

/** messages that mention `q` or belong to item `q`, newest first; without the Hub's search, the loaded ones */
export async function searchThread(q: string, signal?: AbortSignal): Promise<{ msgs: Msg[]; local: boolean }> {
  const x = q.trim()
  if (!x) return { msgs: [], local: false }
  try {
    const r = await modReq<{ messages: RMessage[] }>("GET", "/thread/search" + qs({ q: x }), undefined, signal)
    return { msgs: r.messages.map(msgOf).filter((m) => m.k === "me" || m.k === "steve"), local: false }
  } catch (e) {
    if (signal?.aborted) throw e
    const low = x.toLowerCase()
    const msgs = mainThread(st.thread).filter((m) => (m.k === "me" || m.k === "steve") && ((m.t ?? "").toLowerCase().includes(low) || m.item?.toLowerCase() === low)).reverse()
    return { msgs, local: true }
  }
}

/* ───────── jumping to a message, and back ───────── */
export type Jump = { id: string; created?: number }
const JUMP = "steve-chat-jump"
/** go to a message of the conversation (a search hit, a topic, a quote), from wherever the reader is */
export function jumpToMsg(id: string, created?: number) { window.dispatchEvent(new CustomEvent<Jump>(JUMP, { detail: { id, created } })) }
export function onJump(f: (j: Jump) => void) { const h = (e: Event) => f((e as CustomEvent<Jump>).detail); window.addEventListener(JUMP, h); return () => window.removeEventListener(JUMP, h) }
