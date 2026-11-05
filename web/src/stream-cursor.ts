/* C1: the live stream, resumed from where it stopped.
 * The Hub numbers the frames it keeps (`id: <epoch>:<n>`). Every (re)open asks for what came after the last id this page
 * saw, or, before any id, after the watermark of the state on screen (`x-steve-cursor`). A new EventSource starts with an
 * empty Last-Event-ID, so the cursor goes in the URL (`?after=`) every time. Without a cursor the Hub streams as before
 * and the caller resyncs everything on open (`onOpen(false)`).
 * A phone that comes back to the page (visible, online, pageshow) reconnects at once instead of waiting out the backoff;
 * an open stream that has been silent longer than two pings, or one the browser closed without an error, is reopened too. */

/** the part of EventSource used here, so tests can drive it */
export type SourceLike = {
  readyState: number
  onopen: ((e: any) => void) | null
  onmessage: ((m: any) => void) | null // { data, lastEventId }
  onerror: ((e: any) => void) | null
  addEventListener?: (type: string, f: () => void) => void
  close(): void
}
type Target = { addEventListener(type: string, f: () => void): void; removeEventListener(type: string, f: () => void): void }
export type StreamDeps = {
  /** the stream URL, resuming after `after` when set */
  url: (after: string | null) => string
  source: (url: string) => SourceLike
  /** one frame's data */
  on: (data: string) => void
  /** opened; `resumed` when it continues from a cursor (no full resync needed) */
  onOpen: (resumed: boolean) => void
  /** the watermark of the state on screen, used until a frame id is seen */
  cursor: () => string | null
  setTimeout: (f: () => void, ms: number) => unknown
  clearTimeout: (t: unknown) => void
  now: () => number
  /** window (online, pageshow) and document (visibilitychange); absent in tests that don't need them */
  win?: Target | null
  doc?: (Target & { visibilityState: string }) | null
}

/** Hub pings every 25 s: past this an "open" stream is a dead socket the phone's network stack has not noticed */
export const STALE_MS = 45_000
const FIRST_RETRY = 1000, MAX_RETRY = 15_000, CLOSED = 2

export function resumableStream(d: StreamDeps) {
  let es: SourceLike | null = null, timer: unknown = null, retry = FIRST_RETRY, closed = false
  let last: string | null = null, alive = 0
  const open = () => {
    if (closed) return // a retry scheduled before close must not reopen
    timer = null
    const after = last ?? d.cursor(), src = d.source(d.url(after))
    es = src; alive = d.now()
    src.onopen = () => { retry = FIRST_RETRY; alive = d.now(); d.onOpen(!!after) }
    src.onmessage = (m: { data: string; lastEventId?: string }) => { alive = d.now(); if (m.lastEventId) last = m.lastEventId; d.on(m.data) } // frames without an id keep the cursor
    src.addEventListener?.("ping", () => { alive = d.now() }) // a heartbeat proves the socket, it never moves the cursor
    src.onerror = () => {
      src.close(); if (es !== src) return
      es = null
      if (!closed) timer = d.setTimeout(open, retry = Math.min(retry * 2, MAX_RETRY))
    }
  }
  /* back in front: don't wait for the backoff; a failed attempt now retries after 2 s, not 15 */
  const wake = () => {
    if (closed || d.doc?.visibilityState === "hidden") return
    // a source the browser closed without an error (frozen, bfcache) is dead however recent its last frame
    if (es && es.readyState !== CLOSED && d.now() - alive < STALE_MS) return
    if (es) { const old = es; es = null; old.close() }
    if (timer !== null) { d.clearTimeout(timer); timer = null }
    retry = FIRST_RETRY; open()
  }
  d.doc?.addEventListener("visibilitychange", wake)
  d.win?.addEventListener("online", wake)
  d.win?.addEventListener("pageshow", wake)
  open()
  return {
    close: () => {
      closed = true; es?.close(); es = null
      if (timer !== null) d.clearTimeout(timer)
      d.doc?.removeEventListener("visibilitychange", wake); d.win?.removeEventListener("online", wake); d.win?.removeEventListener("pageshow", wake)
    },
    /** the cursor the next open resumes from */
    cursor: () => last ?? d.cursor(),
  }
}
