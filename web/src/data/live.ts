import { useSyncExternalStore } from "react"
import { qc, scopeKey } from "./sync"
import type { Draft, Msg } from "@/store"
import type { LiveRun } from "@/thread/chat-progress"

type ChatLive = {
  thinking: boolean
  phase: string
  draft: Draft | null
  live: LiveRun | null
  drafted: string
  draftedStart?: number
  sending: Msg[]
}

const idle: ChatLive = { thinking: false, phase: "", draft: null, live: null, drafted: "", sending: [] }
const key = () => ["live", scopeKey()] as const
const listeners = new Set<() => void>()
const subscribe = (fn: () => void) => { listeners.add(fn); return () => listeners.delete(fn) }
export const chatLive = () => qc.getQueryData<ChatLive>(key()) ?? idle
let presented = idle, presentedScope = -1
let pending: { scope: number } | undefined

function present(scope: number) {
  pending = undefined
  const next = chatLive()
  if (presentedScope === scope && next === presented) return
  presented = next; presentedScope = scope
  listeners.forEach((fn) => fn())
}

/** Ingest deltas in arrival order for Hub/stop/handoff logic, but let the UI see at most
 * one live snapshot per frame. A persisted reply, stop or reset publishes immediately. */
export function updateChat(change: Partial<ChatLive>, timing: "now" | "frame" = "now") {
  const scope = scopeKey()
  qc.setQueryData(key(), { ...chatLive(), ...change })
  if (timing === "now") { present(scope); return }
  if (pending?.scope === scope) return
  const frame = pending = { scope }
  requestAnimationFrame(() => {
    if (pending === frame && scope === scopeKey()) present(scope)
  })
}
export function useChatLive<T>(select: (state: ChatLive) => T) {
  return useSyncExternalStore(subscribe, () => select(presentedScope === scopeKey() ? presented : idle))
}
