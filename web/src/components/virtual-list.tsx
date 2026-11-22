import { useVirtualizer } from "@tanstack/react-virtual"
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from "react"

export function VirtualList<T extends { id: string }>({ rows, scroll, render, estimate = 52 }: {
  rows: T[]
  scroll: RefObject<HTMLDivElement | null>
  render: (row: T) => ReactNode
  estimate?: number
}) {
  const list = useRef<HTMLDivElement>(null)
  const [margin, setMargin] = useState(0)
  const [near, setNear] = useState(true)
  const virtual = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scroll.current,
    getItemKey: (index) => rows[index].id,
    estimateSize: () => estimate,
    scrollMargin: margin,
    overscan: 3,
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
  return (
    <div ref={list} className="relative" style={{ height: virtual.getTotalSize() }}>
      {near && virtual.getVirtualItems().map((row) => (
        <div key={row.key} data-index={row.index} ref={virtual.measureElement}
          className="absolute inset-x-0 top-0" style={{ transform: `translateY(${row.start - margin}px)` }}>
          {render(rows[row.index])}
        </div>
      ))}
    </div>
  )
}
