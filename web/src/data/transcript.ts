import { useQueries } from "@tanstack/react-query"
import { useCallback, useEffect, useRef, useSyncExternalStore } from "react"
import { api, Unauthenticated, type REvent } from "../api"
import i18n, { t } from "../i18n"
import { toast } from "../components/ui/toaster"
import { bounded } from "../live-utils"
import { qc, scopeKey } from "./sync"
import { allEvents, appendEvents, mapTurns, type Transcript } from "./transcript-model"

const prefix = ["transcript"] as const
const keyOf = (sid: string) => [...prefix, scopeKey(), sid] as const
const gcTime = 5 * 60_000
qc.setQueryDefaults(prefix, { gcTime, structuralSharing: false })

export const getTranscript = (sid: string) => qc.getQueryData<Transcript>(keyOf(sid))

type Active = {
  uses: number
  controller: AbortController
  buffered: Map<number, REvent>
  gap: boolean
  request?: Promise<void>
  retry?: ReturnType<typeof setTimeout>
  attempts: number
}
const active = new Map<string, Active>()
const idle = new Set<string>()
const transcriptListeners = new Map<string, Set<() => void>>()
let frame = 0

export function onTranscript(sid: string, fn: () => void): () => void {
  let set = transcriptListeners.get(sid)
  if (!set) transcriptListeners.set(sid, (set = new Set()))
  set.add(fn)
  return () => {
    set!.delete(fn)
    if (!set!.size) transcriptListeners.delete(sid)
  }
}

function update(sid: string, f: (data: Transcript | undefined) => Transcript | undefined) {
  const old = getTranscript(sid), data = f(old)
  if (data && data !== old) {
    qc.setQueryData(keyOf(sid), data)
    transcriptListeners.get(sid)?.forEach((fn) => fn())
  }
}

function flush(sid: string, slot: Active) {
  if (slot.gap || slot.request || !slot.buffered.size) return
  const events = [...slot.buffered.values()].sort((a, b) => a.seq - b.seq)
  slot.buffered.clear()
  update(sid, (data) => data && appendEvents(data, events))
}

function recover(sid: string, slot: Active): Promise<void> {
  if (slot.request) return slot.request
  const signal = () => AbortSignal.any([slot.controller.signal, AbortSignal.timeout(30_000)])
  clearTimeout(slot.retry)
  slot.request = (async () => {
    do {
      slot.gap = false
      const data = getTranscript(sid)
      if (!data) {
        const page = await api.transcript(sid, undefined, signal())
        if (slot.controller.signal.aborted) return
        update(sid, () => ({ turns: mapTurns(page.events), oldest: page.next, newest: page.events.at(-1)?.seq ?? 0 }))
      } else {
        let after: number | null = data.newest
        while (after != null) {
          const page = await api.transcriptAfter(sid, after, undefined, signal())
          if (slot.controller.signal.aborted) return
          update(sid, (cached) => cached && appendEvents(cached, page.events))
          after = page.next
        }
      }
    } while (slot.gap)
    slot.attempts = 0
  })().catch((error) => {
    if (slot.controller.signal.aborted || error instanceof Unauthenticated) return
    slot.gap = true
    slot.attempts++
    if (slot.attempts === 3) {
      const msg = error instanceof Error && error.message ? error.message : t("core:error.network")
      toast(t("core:error.loadRetry", { msg }))
    }
    slot.retry = setTimeout(() => void recover(sid, slot), Math.min(30_000, 1000 * 2 ** slot.attempts))
  }).finally(() => {
    slot.request = undefined
    if (!slot.controller.signal.aborted) flush(sid, slot)
  })
  return slot.request
}

export function observeTranscript(sid: string): () => void {
  let slot = active.get(sid)
  if (!slot) {
    idle.delete(sid)
    slot = { uses: 0, controller: new AbortController(), buffered: new Map(), gap: false, attempts: 0 }
    active.set(sid, slot)
    void recover(sid, slot)
  }
  slot.uses++
  return () => {
    if (--slot.uses || active.get(sid) !== slot) return
    slot.controller.abort()
    clearTimeout(slot.retry)
    active.delete(sid)
    if (getTranscript(sid)) idle.add(sid)
    if (idle.size > 5) {
      const oldest = idle.values().next().value!
      idle.delete(oldest)
      qc.removeQueries({ queryKey: keyOf(oldest), exact: true })
    }
  }
}

export function useTranscriptDemand(ids: readonly string[]) {
  const scope = scopeKey(), language = i18n.language, idsKey = JSON.stringify(ids)
  useEffect(() => {
    const releases = (JSON.parse(idsKey) as string[]).map(observeTranscript)
    return () => releases.forEach((release) => release())
  }, [scope, language, idsKey])
}

export function useTranscripts(ids: readonly string[], load: readonly string[] = ids) {
  useTranscriptDemand(load)
  const results = useQueries({
    queries: ids.map((sid) => ({
      queryKey: keyOf(sid), queryFn: () => Promise.resolve(getTranscript(sid)!), enabled: false,
      gcTime, structuralSharing: false,
    })),
  }, qc)
  return results.map((result) => result.data)
}

export const useTranscript = (sid?: string | null) => useTranscripts(sid ? [sid] : [])[0]

const noop = () => {}
/** Subscribe to a derived slice of one session's transcript, re-rendering only when `equal(prev, next)` is false. */
export function useTranscriptSelector<T>(
  sid: string | null | undefined,
  select: (data: Transcript | undefined) => T,
  equal: (a: T, b: T) => boolean = Object.is,
  key?: unknown,
): T {
  useTranscriptDemand(sid ? [sid] : [])
  const selectRef = useRef(select); selectRef.current = select
  const equalRef = useRef(equal); equalRef.current = equal
  const keyRef = useRef(key); keyRef.current = key
  const cache = useRef<{ sid: string | null | undefined; data: Transcript | undefined; key: unknown; value: T; has: boolean }>({
    sid: undefined, data: undefined, key: undefined, value: undefined as T, has: false,
  })
  const subscribe = useCallback((cb: () => void) => (sid ? onTranscript(sid, cb) : noop), [sid])
  const getSnapshot = useCallback(() => {
    const data = sid ? getTranscript(sid) : undefined
    const k = keyRef.current
    const cur = cache.current
    if (cur.has && cur.sid === sid && cur.data === data && Object.is(cur.key, k)) return cur.value
    const next = selectRef.current(data)
    if (cur.has && cur.sid === sid && equalRef.current(cur.value, next)) {
      cur.data = data; cur.key = k
      return cur.value
    }
    cur.sid = sid; cur.data = data; cur.key = k; cur.value = next; cur.has = true
    return next
  }, [sid])
  return useSyncExternalStore(subscribe, getSnapshot)
}

const older = new Map<string, Promise<void>>()
export function loadOlderTranscript(sid: string) {
  const pending = older.get(sid)
  if (pending) return pending
  const before = getTranscript(sid)?.oldest
  if (before == null) return Promise.resolve()
  const scope = scopeKey(), slot = active.get(sid)
  const signal = slot && AbortSignal.any([slot.controller.signal, AbortSignal.timeout(30_000)])
  const request = api.transcript(sid, before, signal).then((page) => {
    if (scope !== scopeKey() || signal?.aborted) return
    update(sid, (data) => data?.oldest === before
      ? { ...data, oldest: page.next, turns: [...mapTurns(page.events), ...data.turns] }
      : data)
  }).finally(() => {
    if (older.get(sid) === request) older.delete(sid)
  })
  older.set(sid, request)
  return request
}

/** Live transcript patches are ordered by their database cursor, which can have holes between rows. */
export function appendTranscript(sid: string, event: REvent) {
  const slot = active.get(sid)
  if (!slot) return
  slot.buffered.set(event.seq, event)
  frame ||= requestAnimationFrame(() => {
    frame = 0
    for (const [sid, slot] of active) flush(sid, slot)
  })
}

export function recoverTranscripts() {
  const entries = [...active]
  // Freeze every affected cursor before a new live frame can pass it.
  for (const [, slot] of entries) slot.gap = true
  return bounded(entries, ([sid, slot]) => active.get(sid) === slot ? recover(sid, slot) : Promise.resolve(), 4)
}

export function resetTranscripts() {
  for (const slot of active.values()) {
    slot.controller.abort()
    clearTimeout(slot.retry)
  }
  active.clear()
  idle.clear()
  older.clear()
  qc.removeQueries({ queryKey: prefix })
  for (const set of transcriptListeners.values()) set.forEach((fn) => fn())
}

i18n.on("languageChanged", resetTranscripts)

export async function readFullTranscript(sid: string) {
  const { marks } = await api.transcriptMarks(sid)
  return mapTurns(await allEvents(marks, (after, through) => api.transcriptAfter(sid, after, through)))
}
