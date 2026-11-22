/* The web client's own performance, for the Hub's health (`steve_web_*`): main-thread tasks over 50 ms and the
 * latency of each interaction (input to next paint: the samples INP is taken from), gathered with
 * PerformanceObserver and sent once a minute when there is something to send. */

/** The most samples of each kind one report carries (what the Hub accepts), and the longest one (ms). */
export const MAX_SAMPLES = 500
const MAX_MS = 60_000

export type PerfReport = { long_tasks: number[]; interactions: number[] }

/** What happened since the last `take`: an interaction counts once, at its slowest event. */
export function collector() {
  let long: number[] = []
  const inter = new Map<number, number>()
  const ms = (v: number) => Math.min(MAX_MS, Math.max(0, Math.round(v)))
  return {
    longTask(duration: number) { if (long.length < MAX_SAMPLES) long.push(ms(duration)) },
    interaction(id: number | undefined, duration: number) {
      if (!id || (!inter.has(id) && inter.size >= MAX_SAMPLES)) return
      inter.set(id, Math.max(inter.get(id) ?? 0, ms(duration)))
    },
    take(): PerfReport | null {
      if (!long.length && !inter.size) return null
      const r = { long_tasks: long, interactions: [...inter.values()] }
      long = []
      inter.clear()
      return r
    },
  }
}

/** Observes this page for as long as it is open; `send` posts a report (failures are dropped with it). */
export function startPerf(send: (r: PerfReport) => Promise<unknown>, every = 60_000) {
  if (typeof PerformanceObserver === "undefined") return
  const c = collector()
  const observe = (type: string, init: object, f: (e: PerformanceEntry) => void) => {
    try { new PerformanceObserver((list) => list.getEntries().forEach(f)).observe({ type, buffered: true, ...init } as PerformanceObserverInit) } catch { /* a browser without this entry type */ }
  }
  observe("longtask", {}, (e) => c.longTask(e.duration))
  observe("event", { durationThreshold: 40 }, (e) => c.interaction((e as PerformanceEventTiming).interactionId, e.duration))
  setInterval(() => { const r = c.take(); if (r) void send(r).catch(() => {}) }, every)
}
