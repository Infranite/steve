import { useT } from "@/i18n"
import { useVirtualizer } from "@tanstack/react-virtual"
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from "react"

export function VirtualList<T extends { id: string }>({ rows, scroll, render, estimate = 52, end = false, earlier }: {
  rows: T[]
  scroll: RefObject<HTMLDivElement | null>
  render: (row: T) => ReactNode
  estimate?: number | ((row: T) => number)
  end?: boolean
  earlier?: { load: () => Promise<void> }
}) {
  const list = useRef<HTMLDivElement>(null)
  const [margin, setMargin] = useState(0)
  const [near, setNear] = useState(true)
  const [older, setOlder] = useState({ loading: false, failed: false })
  const positioned = useRef(false)
  const loadingOlder = useRef(false)
  const virtual = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scroll.current,
    getItemKey: (index) => rows[index].id,
    estimateSize: (index) => typeof estimate === "number" ? estimate : estimate(rows[index]),
    scrollMargin: margin,
    overscan: 3,
    anchorTo: end ? "end" : "start",
    followOnAppend: end,
    scrollEndThreshold: 48,
    initialRect: { width: 0, height: 680 },
  })
  useLayoutEffect(() => {
    const viewport = scroll.current, el = list.current
    if (!viewport || !el) return
    const measure = () => setMargin(el.getBoundingClientRect().top - viewport.getBoundingClientRect().top + viewport.scrollTop)
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(viewport)
    if (viewport.firstElementChild) observer.observe(viewport.firstElementChild)
    return () => observer.disconnect()
  }, [scroll, rows.length])
  useEffect(() => {
    const viewport = scroll.current, el = list.current
    if (!viewport || !el) return
    const observer = new IntersectionObserver(([entry]) => setNear(entry.isIntersecting), {
      root: viewport,
      rootMargin: "96px 0px",
    })
    observer.observe(el)
    return () => observer.disconnect()
  }, [scroll])
  useLayoutEffect(() => {
    if (!end || positioned.current || !rows.length || !scroll.current) return
    positioned.current = true
    virtual.scrollToEnd()
  }, [end, rows.length, scroll, virtual, margin])

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
    const viewport = scroll.current
    if (!earlier || !viewport || older.failed) return
    const nearStart = () => {
      if (viewport.scrollTop <= margin + 240 && viewport.scrollTop < viewport.scrollHeight - viewport.clientHeight - 12) loadEarlier()
    }
    viewport.addEventListener("scroll", nearStart, { passive: true })
    return () => viewport.removeEventListener("scroll", nearStart)
  }, [earlier, margin, older.failed, scroll]) // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <div ref={list} className="relative" style={{ height: virtual.getTotalSize(), overflowAnchor: end ? "none" : undefined }}>
      {earlier && <Earlier loading={older.loading} retry={older.failed} load={loadEarlier} />}
      {near && virtual.getVirtualItems().map((row) => (
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
