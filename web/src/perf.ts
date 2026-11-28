/* The web client's own performance, errors, SSE reconnects, and navigation usage, for the Hub's telemetry (`steve_web_*`):
 * main-thread tasks over 50 ms, the latency of each interaction (input to next paint: the samples INP is taken from),
 * gathered with PerformanceObserver, how long merging each patch frame took, menu item open counts
 * (`steve_web_nav_opens_total`), uncaught JS errors (`steve_web_errors_total`), and SSE reconnect counts
 * (`steve_web_sse_reconnects_total`); sent via the one `POST /api/system/web-perf` pipeline. */
/* relative .ts import so node --test loads this file without a bundler */
import { navItemOf, type NavItemKey } from "./nav.ts"
import type { ReconnectReason } from "./stream-cursor.ts"

/** The most samples of each kind one report carries (what the Hub accepts), and the longest one (ms). */
export const MAX_SAMPLES = 500
const MAX_MS = 60_000

export type WebErrorKind = "error" | "unhandledrejection"

export type PerfReport = {
  page?: Page
  long_tasks: number[]
  interactions: number[]
  patch_apply?: number[]
  snapshots?: number[]
  transcript_open?: number[]
  dom_nodes?: number
  nav_opens?: Record<string, number>
  errors?: Record<string, number>
  sse_reconnects?: Record<string, number>
}

/** Known non-sidebar routes mapped into the bounded Hub WEB_PAGES set so zero known routes fall into "other". */
const EXTRA_PAGE_ROUTES: Record<string, Page> = {
  more: "settings",
  design: "settings",
  hub: "devices",
  d: "publications",
}

/** The pages the Hub labels figures with (a bounded set: steve-hub `metrics::WEB_PAGES`). */
export type Page = NavItemKey | "item" | "session" | "other"
export function pageOf(hash: string): Page {
  const clean = hash.replace(/^#\/?/, "").split("?")[0]
  const [a = "", b = ""] = clean.split("/")
  if (!a) return "chat"
  if (a === "items") return b ? "item" : "items"
  if (a === "sessions") return b ? "session" : "sessions"
  const nav = navItemOf(hash)
  if (nav) return nav
  return Object.prototype.hasOwnProperty.call(EXTRA_PAGE_ROUTES, a) ? EXTRA_PAGE_ROUTES[a] : "other"
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
const pendingErrors = new Map<WebErrorKind, number>()
const pendingReconnects = new Map<ReconnectReason, number>()
let inflightNav: Promise<void> = Promise.resolve()
let globalErrorListenersAttached = false

function takeMap<K extends string>(m: Map<K, number>): Record<string, number> | undefined {
  if (!m.size) return undefined
  const out: Record<string, number> = {}
  for (const [k, v] of m) out[k] = v
  m.clear()
  return out
}

function takeNavOpens(): Record<string, number> | undefined {
  return takeMap(pendingNav)
}

/** Records one uncaught browser error or unhandled promise rejection (bounded label). */
export function recordWebError(kind: WebErrorKind) {
  pendingErrors.set(kind, (pendingErrors.get(kind) ?? 0) + 1)
}

/** Records one SSE reconnect attempt (bounded reason: error, stale, wake). */
export function recordSseReconnect(reason: ReconnectReason) {
  pendingReconnects.set(reason, (pendingReconnects.get(reason) ?? 0) + 1)
}

/** Flushes queued navigation open, JS error, and SSE reconnect counts through the registered web telemetry sender. */
export function flushNavOpens(): Promise<void> {
  if (!navSender || (!pendingNav.size && !pendingErrors.size && !pendingReconnects.size)) return inflightNav
  const nav_opens = takeNavOpens()
  const errors = takeMap(pendingErrors)
  const sse_reconnects = takeMap(pendingReconnects)
  if (!nav_opens && !errors && !sse_reconnects) return inflightNav
  const page = typeof location === "undefined" ? "other" : pageOf(location.hash)
  const send = navSender
  inflightNav = inflightNav.then(() => send({
    page,
    long_tasks: [],
    interactions: [],
    ...(nav_opens ? { nav_opens } : {}),
    ...(errors ? { errors } : {}),
    ...(sse_reconnects ? { sse_reconnects } : {}),
  }).then(() => {}, () => {}))
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

/** Resets in-memory navigation, error, and reconnect tracking state (for tests). */
export function resetNavTracker(sender: ((r: PerfReport) => Promise<unknown>) | null = null) {
  navSender = sender
  lastNav = null
  pendingNav.clear()
  pendingErrors.clear()
  pendingReconnects.clear()
  inflightNav = Promise.resolve()
}

/** Observes this page for as long as it is open; `send` posts a report (failures are dropped with it). */
export function startPerf(send: (r: PerfReport) => Promise<unknown>, every = 60_000, patches: () => number[] = () => [], snapshots: () => number[] = () => []) {
  navSender = send
  if (!globalErrorListenersAttached && typeof window !== "undefined") {
    globalErrorListenersAttached = true
    window.addEventListener("error", () => recordWebError("error"))
    window.addEventListener("unhandledrejection", () => recordWebError("unhandledrejection"))
  }
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
    const errors = takeMap(pendingErrors)
    const sse_reconnects = takeMap(pendingReconnects)
    const dom = sampleDom ? document.getElementsByTagName("*").length : undefined
    if (r || p.length || s.length || opens.length || nav_opens || errors || sse_reconnects || dom != null) {
      void send({
        ...(r ?? { long_tasks: [], interactions: [] }),
        page,
        patch_apply: p,
        snapshots: s,
        transcript_open: opens,
        dom_nodes: dom,
        ...(nav_opens ? { nav_opens } : {}),
        ...(errors ? { errors } : {}),
        ...(sse_reconnects ? { sse_reconnects } : {}),
      }).catch((e) => console.warn("perf report failed", e))
    }
  }
  addEventListener("hashchange", () => {
    const next = pageOf(location.hash)
    if (next !== page) { flush(); page = next }
    else if (pendingNav.size || pendingErrors.size || pendingReconnects.size) flush()
  })
  if (typeof document !== "undefined") {
    document.addEventListener("visibilitychange", () => { if (document.visibilityState === "hidden") flush() })
  }
  setInterval(() => flush(true), every)
}
