import { useSyncExternalStore } from "react"
import { qc, scopeKey } from "./sync"
import type { ThreadResearchStage } from "@/gen"
import type { Draft, Msg } from "@/store"
import type { LiveRun } from "@/thread/chat-progress"

type ChatLive = {
  thinking: boolean
  thinkingStartedAt?: number
  thinkingAt?: number
  thinkingStage?: ThreadResearchStage
  phase: string
  draft: Draft | null
  live: LiveRun | null
  drafted: string
  draftedStart?: number
  sending: Msg[]
}

const idle: ChatLive = { thinking: false, thinkingStartedAt: undefined, thinkingAt: undefined, thinkingStage: undefined, phase: "", draft: null, live: null, drafted: "", sending: [] }
const key = () => ["live", scopeKey()] as const
const listeners = new Set<() => void>()
export const observeChatLive = (fn: () => void) => { listeners.add(fn); return () => { listeners.delete(fn) } }
export const chatLive = () => qc.getQueryData<ChatLive>(key()) ?? idle
let presented = idle, presentedScope = -1
export const REPLY_BATCH_MS = 100
type DeferredTiming = "frame" | "reply"
let pending: { scope: number; timing: DeferredTiming; timer?: ReturnType<typeof setTimeout> } | undefined

export const presentedChatLive = () => presentedScope === scopeKey() ? presented : idle

function cancelPending() {
  if (pending?.timer !== undefined) clearTimeout(pending.timer)
  pending = undefined
}

function present(scope: number) {
  cancelPending()
  const next = chatLive()
  if (presentedScope === scope && next === presented) return
  presented = next; presentedScope = scope
  listeners.forEach((fn) => fn())
}

/** Ingest deltas in order for Hub/stop/handoff logic. Only ordinary body
 * chunks wait for the batch; tool/phase changes and persistence can overtake it. */
export function updateChat(change: Partial<ChatLive>, timing: "now" | DeferredTiming = "now") {
  const scope = scopeKey()
  qc.setQueryData(key(), { ...chatLive(), ...change })
  if (timing === "now") { present(scope); return }
  if (pending?.scope === scope && (timing === "reply" || pending.timing === "frame")) return
  cancelPending()
  const batch: NonNullable<typeof pending> = pending = { scope, timing }
  const flush = () => {
    if (pending === batch && scope === scopeKey()) present(scope)
  }
  if (timing === "frame") requestAnimationFrame(flush)
  else batch.timer = setTimeout(flush, REPLY_BATCH_MS)
}
export function useChatLive<T>(select: (state: ChatLive) => T) {
  return useSyncExternalStore(observeChatLive, () => select(presentedChatLive()))
}
