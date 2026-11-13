/* 统计页的分节和表格（stats v2）：一节是一个看得见的标题、一行说明和它的内容，节与节之间靠留白分开；
 * 表格只在头下面一条线、行之间细线，手机上左右滑。 */
import { useLayoutEffect, useRef, useState, type ReactNode } from "react"
import { cn } from "cn"

export const th = "whitespace-nowrap py-2 type-footnote font-medium text-muted-foreground"
export const num = "whitespace-nowrap py-2.5 pl-4 text-right tabular-nums"

/** a part of the page: its heading (what question it answers), one grey line under it, its own switches on the right */
export function Section({ title, sub, right, children, k, className }: { title: ReactNode; sub?: ReactNode; right?: ReactNode; children: ReactNode; k: string; className?: string }) {
  return (
    <section className={cn("flex min-w-0 flex-col gap-4", className)} data-section={k} aria-labelledby={`stats-h-${k}`}>
      <header className="flex flex-wrap items-end justify-between gap-x-4 gap-y-2">
        <div className="min-w-0">
          <h2 id={`stats-h-${k}`} className="type-title text-foreground">{title}</h2>
          {sub && <p className="mt-0.5 max-w-[72ch] type-callout text-pretty text-muted-foreground">{sub}</p>}
        </div>
        {right && <div className="flex flex-wrap items-center gap-2">{right}</div>}
      </header>
      {children}
    </section>
  )
}

/** a table inside a panel: a rule under its head and hairlines only between rows, scrolling sideways on a phone. While more
 *  columns lie to the right, the right edge fades out (the cue that it scrolls); `stick` keeps the first column in place */
export function Table({ head, children, label, min = 560, stick = false }: { head: ReactNode; children: ReactNode; label: string; min?: number; stick?: boolean }) {
  const box = useRef<HTMLDivElement>(null)
  const [more, setMore] = useState(false)
  useLayoutEffect(() => {
    const el = box.current; if (!el) return
    const check = () => setMore(el.scrollLeft + el.clientWidth < el.scrollWidth - 1)
    const ro = new ResizeObserver(check); ro.observe(el); check()
    el.addEventListener("scroll", check, { passive: true })
    return () => { ro.disconnect(); el.removeEventListener("scroll", check) }
  }, [])
  return (
    <div ref={box} className={cn("overflow-x-auto overscroll-x-contain px-5", more && "[mask-image:linear-gradient(to_right,black_calc(100%-40px),transparent)]")} data-table-scroll={more || undefined}>
      <table className={cn("w-full type-callout", stick && "[&_tr>:first-child]:sticky [&_tr>:first-child]:left-0 [&_tr>:first-child]:z-[1] [&_tr>:first-child]:bg-card")} style={{ minWidth: min }} aria-label={label}>
        <thead className="border-b text-left">{head}</thead>
        <tbody className="[&>tr]:border-b [&>tr]:border-border/60 [&>tr:last-child]:border-0">{children}</tbody>
      </table>
    </div>
  )
}
