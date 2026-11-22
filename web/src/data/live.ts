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
export function updateChat(change: Partial<ChatLive>) {
  qc.setQueryData(key(), { ...chatLive(), ...change })
  listeners.forEach((fn) => fn())
}
export function useChatLive<T>(select: (state: ChatLive) => T) {
  return useSyncExternalStore(subscribe, () => select(chatLive()))
}
