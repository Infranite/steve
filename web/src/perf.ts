/* The web client's own performance and navigation usage, for the Hub's telemetry (`steve_web_*`): main-thread tasks
 * over 50 ms, the latency of each interaction (input to next paint: the samples INP is taken from), gathered with
 * PerformanceObserver, how long merging each patch frame took, and menu item open counts (`steve_web_nav_opens_total`);
 * sent via the one `POST /api/system/web-perf` pipeline. */
/* relative .ts import so node --test loads this file without a bundler */
import { navItemOf, type NavItemKey } from "./nav.ts"

/** The most samples of each kind one report carries (what the Hub accepts), and the longest one (ms). */
export const MAX_SAMPLES = 500
const MAX_MS = 60_000

export type PerfReport = {
  page?: Page
  long_tasks: number[]
  interactions: number[]
  patch_apply?: number[]
  snapshots?: number[]
  transcript_open?: number[]
  dom_nodes?: number
  nav_opens?: Record<string, number>
}

/** The pages the Hub labels figures with (a bounded set: steve-hub `metrics::WEB_PAGES`). */
export type Page = "chat" | "items" | "item" | "sessions" | "session" | "inbox" | "other"
export function pageOf(hash: string): Page {
  const [a, b] = hash.replace(/^#\/?/, "").split(/[/?]/)
  if (!a) return "chat"
  if (a === "inbox") return "inbox"
  if (a === "items") return b ? "item" : "items"
  if (a === "sessions") return b ? "session" : "sessions"
  return "other"
}

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

const transcriptOpens: number[] = []
export function recordTranscriptOpen(ms: number) {
  if (transcriptOpens.length < MAX_SAMPLES) transcriptOpens.push(Math.min(ms, MAX_MS))
}

let navSender: ((r: PerfReport) => Promise<unknown>) | null = null
let lastNav: NavItemKey | null = null
const pendingNav = new Map<NavItemKey, number>()
let inflightNav: Promise<void> = Promise.resolve()

function takeNavOpens(): Record<string, number> | undefined {
  if (!pendingNav.size) return undefined
  const out: Record<string, number> = {}
  for (const [k, v] of pendingNav) out[k] = v
  pendingNav.clear()
  return out
}

/** Flushes queued navigation open counts through the registered web telemetry sender. */
export function flushNavOpens(): Promise<void> {
  if (!navSender || !pendingNav.size) return inflightNav
  const nav_opens = takeNavOpens()
  if (!nav_opens) return inflightNav
  const page = typeof location === "undefined" ? "other" : pageOf(location.hash)
  const send = navSender
  inflightNav = inflightNav.then(() => send({ page, long_tasks: [], interactions: [], nav_opens }).then(() => {}, () => {}))
  return inflightNav
}

/** Records one open of the destination resolved from `hash`, deduplicating consecutive hits to the same menu item
 *  (sub-route navigation like `#/items` -> `#/items/ST-1` and React StrictMode double-mounts count once). Counts are
 *  buffered and sent on the existing `startPerf` flushes (route change, periodic tick, tab hide, or `flushNavOpens`). */
export function recordNavOpen(hash: string): boolean {
  const item = navItemOf(hash)
  if (!item || item === lastNav) return false
  lastNav = item
  pendingNav.set(item, (pendingNav.get(item) ?? 0) + 1)
  return true
}

/** Resets in-memory navigation tracking state (for tests). */
export function resetNavTracker(sender: ((r: PerfReport) => Promise<unknown>) | null = null) {
  navSender = sender
  lastNav = null
  pendingNav.clear()
  inflightNav = Promise.resolve()
}

/** Observes this page for as long as it is open; `send` posts a report (failures are dropped with it). */
export function startPerf(send: (r: PerfReport) => Promise<unknown>, every = 60_000, patches: () => number[] = () => [], snapshots: () => number[] = () => []) {
  navSender = send
  if (typeof PerformanceObserver === "undefined") return
  const c = collector()
  const observe = (type: string, init: object, f: (e: PerformanceEntry) => void) => {
    try { new PerformanceObserver((list) => list.getEntries().forEach(f)).observe({ type, buffered: true, ...init } as PerformanceObserverInit) } catch { /* a browser without this entry type */ }
  }
  observe("longtask", {}, (e) => c.longTask(e.duration))
  observe("event", { durationThreshold: 40 }, (e) => c.interaction((e as PerformanceEventTiming).interactionId, e.duration))
  let page = pageOf(location.hash)
  const samples = (xs: number[]) => xs.slice(0, MAX_SAMPLES).map((ms) => Math.min(MAX_MS, Math.max(0, ms)))
  const flush = (sampleDom = false) => {
    const r = c.take(), p = samples(patches()), s = samples(snapshots())
    const opens = transcriptOpens.splice(0)
    const nav_opens = takeNavOpens()
    const dom = sampleDom ? document.getElementsByTagName("*").length : undefined
    if (r || p.length || s.length || opens.length || nav_opens || dom != null) void send({ ...(r ?? { long_tasks: [], interactions: [] }), page, patch_apply: p, snapshots: s, transcript_open: opens, dom_nodes: dom, ...(nav_opens ? { nav_opens } : {}) }).catch(() => {})
  }
  addEventListener("hashchange", () => {
    const next = pageOf(location.hash)
    if (next !== page) { flush(); page = next }
    else if (pendingNav.size) flush()
  })
  if (typeof document !== "undefined") {
    document.addEventListener("visibilitychange", () => { if (document.visibilityState === "hidden") void flushNavOpens() })
  }
  setInterval(() => flush(true), every)
}
