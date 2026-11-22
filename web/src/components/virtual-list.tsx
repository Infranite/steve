import { useT } from "@/i18n"
import { useVirtualizer, type Virtualizer } from "@tanstack/react-virtual"
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from "react"

export function VirtualList<T extends { id: string }>({ rows, scroll, render, estimate = 52, gap = 0, startAtEnd = false, handle, end = false, earlier }: {
  rows: T[]
  scroll: RefObject<HTMLDivElement | null>
  render: (row: T) => ReactNode
  estimate?: number | ((row: T) => number)
  gap?: number
  startAtEnd?: boolean
  handle?: RefObject<Virtualizer<HTMLDivElement, HTMLDivElement> | null>
  end?: boolean
  earlier?: { load: () => Promise<void> }
}) {
  const list = useRef<HTMLDivElement>(null)
  const [viewport, setViewport] = useState<HTMLDivElement | null>(null)
  // The parent viewport can mount in the same commit as an already cached list.
  useEffect(() => { setViewport(scroll.current) }, [scroll])
  const [margin, setMargin] = useState<number | undefined>(() => handle ? 0 : undefined)
  const [near, setNear] = useState(true)
  const [older, setOlder] = useState({ loading: false, failed: false })
  const positioned = useRef(false)
  const loadingOlder = useRef(false)
  const keyOf = useCallback((index: number) => rows[index].id, [rows])
  const sizeOf = useCallback((index: number) => typeof estimate === "number" ? estimate : estimate(rows[index]), [estimate, rows])
  const virtual = useVirtualizer<HTMLDivElement, HTMLDivElement>({
    count: rows.length,
    getScrollElement: () => scroll.current,
    getItemKey: keyOf,
    estimateSize: sizeOf,
    scrollMargin: margin ?? 0,
    gap,
    initialOffset: () => startAtEnd ? Math.max(0, rows.reduce((n, _, index) => n + sizeOf(index) + gap, 0) - 560) : 0,
    overscan: 3,
    anchorTo: end ? "end" : "start",
    followOnAppend: end,
    scrollEndThreshold: 48,
    initialRect: { width: 0, height: 680 },
    useAnimationFrameWithResizeObserver: !!handle,
  })
  useLayoutEffect(() => { if (handle) { handle.current = virtual; return () => { handle.current = null } } }, [handle, virtual])
  useLayoutEffect(() => {
    const el = list.current
    if (!viewport || !el) return
    const measure = () => setMargin(el.getBoundingClientRect().top - viewport.getBoundingClientRect().top + viewport.scrollTop)
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(viewport)
    if (viewport.firstElementChild) observer.observe(viewport.firstElementChild)
    return () => observer.disconnect()
  }, [viewport, rows.length])
  useEffect(() => {
    const el = list.current
    if (!viewport || !el) return
    const observer = new IntersectionObserver(([entry]) => setNear(entry.isIntersecting), {
      root: viewport,
      rootMargin: "96px 0px",
    })
    observer.observe(el)
    return () => observer.disconnect()
  }, [viewport])
  useLayoutEffect(() => {
    if (!end || positioned.current || !rows.length || !viewport || margin === undefined) return
    positioned.current = true
    virtual.scrollToEnd()
  }, [end, rows.length, viewport, virtual, margin])

  const loadEarlier = () => {
    if (!earlier || loadingOlder.current) return
    loadingOlder.current = true
    setOlder({ loading: true, failed: false })
    void earlier.load().then(
      () => setOlder({ loading: false, failed: false }),
      () => setOlder({ loading: false, failed: true }),
    ).finally(() => { loadingOlder.current = false })
  }
  useEffect(() => {
    if (!earlier || !viewport || older.failed) return
    const nearStart = () => {
      if (viewport.scrollTop <= (margin ?? 0) + 240 && viewport.scrollTop < viewport.scrollHeight - viewport.clientHeight - 12) loadEarlier()
    }
    viewport.addEventListener("scroll", nearStart, { passive: true })
    return () => viewport.removeEventListener("scroll", nearStart)
  }, [earlier, margin, older.failed, viewport]) // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div ref={list} className="relative" style={{ height: virtual.getTotalSize(), overflowAnchor: end ? "none" : undefined }}>
      {earlier && <Earlier loading={older.loading} retry={older.failed} load={loadEarlier} />}
      {margin !== undefined && near && virtual.getVirtualItems().map((row) => (
        <div key={row.key} data-index={row.index} ref={virtual.measureElement}
          className="absolute inset-x-0 top-0" style={{ transform: `translateY(${row.start - margin}px)` }}>
          {render(rows[row.index])}
        </div>
      ))}
    </div>
  )
}

function Earlier({ loading, retry, load }: { loading: boolean; retry: boolean; load: () => void }) {
  const t = useT()
  return <button type="button" onClick={load} disabled={loading} data-transcript-earlier
    className="absolute bottom-full left-0 pb-2 type-footnote text-muted-foreground hover:text-foreground">
    {t(loading ? "common:state.loading" : retry ? "common:action.retry" : "core:transcript.earlier")}
  </button>
}
