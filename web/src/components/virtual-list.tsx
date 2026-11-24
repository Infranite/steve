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
    // Chat owns follow-to-latest (its draft is outside this list), but the
    // virtualizer must resolve a prepended key before it renders the new range.
    anchorTo: end || handle ? "end" : "start",
    followOnAppend: end,
    scrollEndThreshold: handle ? 0 : 48,
    initialRect: { width: 0, height: 680 },
    // Commit the measured positions and scroll correction in the frame's
    // animation callbacks. A scrollTop write during ResizeObserver delivery
    // can take effect a paint before the row's new geometry in Chromium.
    directDomUpdates: !!handle,
    directDomUpdatesMode: handle ? "position" : "transform",
    useAnimationFrameWithResizeObserver: !!handle,
    useFlushSync: !handle,
  })
  const setList = useCallback((el: HTMLDivElement | null) => {
    list.current = el
    if (handle) virtual.containerRef(el)
  }, [handle, virtual])
  useLayoutEffect(() => {
    if (!handle) return
    handle.current = virtual
    // A history row that changes entirely above the viewport must compensate
    // in either wheel direction. Direct writes above commit the compensation
    // together with the new row positions; an intersecting row grows in place.
    virtual.shouldAdjustScrollPositionOnItemSizeChange = (row, _delta, instance) =>
      row.end <= (instance.scrollOffset ?? 0)
    return () => {
      handle.current = null
      virtual.shouldAdjustScrollPositionOnItemSizeChange = undefined
    }
  }, [handle, virtual])
  useLayoutEffect(() => {
    const el = list.current
    if (!viewport || !el) return
    const measure = () => setMargin(el.getBoundingClientRect().top - viewport.getBoundingClientRect().top + viewport.scrollTop)
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(viewport)
    if (handle) {
      // A growing draft after the list doesn't change its start. Only measure
      // siblings above it (history controls) and viewport size changes.
      for (let above = el.previousElementSibling; above; above = above.previousElementSibling) observer.observe(above)
    } else if (viewport.firstElementChild) observer.observe(viewport.firstElementChild)
    return () => observer.disconnect()
  }, [viewport, rows.length, handle])
  useEffect(() => {
    const el = list.current
    if (!viewport || !el || handle) return
    const observer = new IntersectionObserver(([entry]) => setNear(entry.isIntersecting), {
      root: viewport,
      rootMargin: "96px 0px",
    })
    observer.observe(el)
    return () => observer.disconnect()
  }, [viewport, handle])
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
    <div ref={setList} className="relative" style={{ height: handle ? undefined : virtual.getTotalSize(), overflowAnchor: end ? "none" : undefined }}>
      {earlier && <Earlier loading={older.loading} retry={older.failed} load={loadEarlier} />}
      {margin !== undefined && (handle || near) && virtual.getVirtualItems().map((row) => (
        <div key={row.key} data-index={row.index} ref={virtual.measureElement}
          className="absolute inset-x-0 top-0" style={handle ? undefined : { transform: `translateY(${row.start - margin}px)` }}>
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
