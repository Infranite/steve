/* 趋势图（stats.md §6.2–6.3）：单一纵轴；默认正文灰的明度梯度，悬停/选中的那一天用 accent 蓝；
 * 堆叠柱（按天）、折线（命中率）、中位数折线带 p85 浅带（前置时间）。十字线 + 提示框；点一天回调；每个图可切表格。 */
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react"
import { cn } from "cn"
import type { Series } from "@/api/stats"
import { shortDay } from "@/stats/format"

/** grey steps of the text colour, darkest first: series are told apart by position, legend and labels, not hue */
export const GREYS = [0.82, 0.52, 0.32, 0.18, 0.66, 0.42]
const grey = (i: number) => `color-mix(in oklab, var(--foreground) ${Math.round(GREYS[i % GREYS.length] * 100)}%, var(--background))`
export const Swatch = ({ i, line }: { i: number; line?: boolean }) => <span aria-hidden className={cn("inline-block shrink-0", line ? "h-0.5 w-3 rounded-full" : "size-2.5 rounded-[3px]")} style={{ background: grey(i) }} />

export type Kind = "stack" | "line" | "band"
const H = 220, PAD = { l: 44, r: 12, t: 12, b: 26 }

function niceMax(v: number) {
  if (v <= 0) return 1
  const p = 10 ** Math.floor(Math.log10(v)), m = v / p
  return (m <= 1 ? 1 : m <= 2 ? 2 : m <= 2.5 ? 2.5 : m <= 5 ? 5 : 10) * p
}

export function Chart({ data, kind, fmt, picked, onPick, label }: {
  data: Series; kind: Kind; fmt: (v: number) => string; picked?: string | null; onPick?: (day: string | null) => void; label: string
}) {
  const box = useRef<HTMLDivElement>(null)
  const [w, setW] = useState(640)
  const [hover, setHover] = useState<number | null>(null)
  useEffect(() => {
    const el = box.current; if (!el) return
    const ro = new ResizeObserver(() => setW(el.clientWidth || 640)); ro.observe(el); setW(el.clientWidth || 640)
    return () => ro.disconnect()
  }, [])
  const n = data.buckets.length, lines = data.series
  const iw = Math.max(1, w - PAD.l - PAD.r), ih = H - PAD.t - PAD.b, slot = iw / Math.max(1, n)
  const stackTop = useMemo(() => data.buckets.map((_, i) => lines.reduce((s, l) => s + (l.values[i] ?? 0), 0)), [data, lines])
  const max = niceMax(kind === "stack" ? Math.max(0, ...stackTop) : Math.max(0, ...lines.flatMap((l) => l.values.map((v) => v ?? 0))))
  const y = (v: number) => PAD.t + ih - (v / max) * ih
  const x = (i: number) => PAD.l + slot * i + slot / 2
  const ticks = [0, 0.5, 1].map((t) => t * max)
  const every = Math.max(1, Math.ceil(n / Math.max(2, Math.floor(iw / 56))))
  const bw = Math.min(24, Math.max(3, slot * 0.62))
  const at = hover ?? (picked ? data.buckets.indexOf(picked) : -1)
  const path = (vals: (number | null)[]) => {
    let d = "", pen = false
    vals.forEach((v, i) => { if (v == null) { pen = false; return } d += (pen ? "L" : "M") + x(i).toFixed(1) + "," + y(v).toFixed(1); pen = true })
    return d
  }
  const tip: ReactNode = at >= 0 && at < n && (
    <div role="status" className="pointer-events-none absolute top-1 z-10 min-w-36 rounded-lg border bg-popover px-3 py-2 text-popover-foreground shadow-md"
      style={{ left: Math.min(Math.max(0, x(at) + 12), Math.max(0, w - 180)) }} data-chart-tip>
      <div className="type-label text-muted-foreground">{data.buckets[at]}</div>
      {lines.map((l, i) => (
        <div key={l.key} className="mt-1 flex items-center gap-2 type-footnote"><Swatch i={i} line={kind !== "stack"} /><span className="min-w-0 flex-1 truncate">{l.label}</span><span className="tabular-nums">{l.values[at] == null ? "—" : fmt(l.values[at]!)}</span></div>
      ))}
      {kind === "stack" && lines.length > 1 && <div className="mt-1 flex justify-between border-t pt-1 type-footnote"><span>合计</span><span className="tabular-nums">{fmt(stackTop[at])}</span></div>}
    </div>
  )
  return (
    <div ref={box} className="relative w-full select-none" data-chart={kind}>
      <svg width={w} height={H} role="img" aria-label={label} className="block overflow-visible"
        onMouseLeave={() => setHover(null)}
        onMouseMove={(e) => { const r = e.currentTarget.getBoundingClientRect(); const i = Math.floor((e.clientX - r.left - PAD.l) / slot); setHover(i >= 0 && i < n ? i : null) }}
        onClick={() => onPick?.(hover != null && data.buckets[hover] !== picked ? data.buckets[hover] : null)}>
        {ticks.map((t) => (
          <g key={t}>
            <line x1={PAD.l} x2={w - PAD.r} y1={y(t)} y2={y(t)} stroke="var(--border)" strokeWidth={1} />
            <text x={PAD.l - 8} y={y(t)} dy="0.32em" textAnchor="end" className="fill-muted-foreground text-[11px] tabular-nums">{fmt(t)}</text>
          </g>
        ))}
        {data.buckets.map((d, i) => i % every === 0 && <text key={d} x={x(i)} y={H - 8} textAnchor="middle" className="fill-muted-foreground text-[11px] tabular-nums">{shortDay(d)}</text>)}
        {at >= 0 && at < n && <line x1={x(at)} x2={x(at)} y1={PAD.t} y2={PAD.t + ih} stroke="var(--border)" strokeWidth={1} />}
        {kind === "stack" && data.buckets.map((d, i) => {
          let base = 0
          const on = i === at
          return (
            <g key={d} data-bar={d}>
              {lines.map((l, k) => {
                const v = l.values[i] ?? 0
                if (v <= 0) return null
                const y0 = y(base), y1 = y(base + v); base += v
                const top = lines.slice(k + 1).every((m) => (m.values[i] ?? 0) <= 0)
                const h = Math.max(1, y0 - y1 - (top ? 0 : 2)) // a 2px surface gap between segments
                const r = top ? Math.min(4, h / 2, bw / 2) : 0
                const xl = x(i) - bw / 2, yt = y0 - h
                return <path key={l.key} fill={on ? (k === 0 ? "var(--blue)" : `color-mix(in oklab, var(--blue) ${Math.round(GREYS[k % GREYS.length] * 100)}%, var(--background))`) : grey(k)}
                  d={`M${xl},${y0}V${yt + r}Q${xl},${yt} ${xl + r},${yt}H${xl + bw - r}Q${xl + bw},${yt} ${xl + bw},${yt + r}V${y0}Z`} />
              })}
            </g>
          )
        })}
        {kind === "band" && lines.length >= 2 && (() => {
          const lo = lines[0].values, hi = lines[1].values
          const pts = data.buckets.map((_, i) => i).filter((i) => lo[i] != null && hi[i] != null)
          if (!pts.length) return null
          const d = pts.map((i, k) => (k ? "L" : "M") + x(i) + "," + y(hi[i]!)).join("") + pts.slice().reverse().map((i) => "L" + x(i) + "," + y(lo[i]!)).join("") + "Z"
          return <path d={d} fill="var(--foreground)" opacity={0.08} />
        })()}
        {kind !== "stack" && lines.slice(0, kind === "band" ? 1 : undefined).map((l, k) => (
          <g key={l.key}>
            <path d={path(l.values)} fill="none" stroke={grey(k)} strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
            {l.values.map((v, i) => v != null && (i === at || l.values.filter((z) => z != null).length === 1) && (
              <circle key={i} cx={x(i)} cy={y(v)} r={4} fill={i === at ? "var(--blue)" : grey(k)} stroke="var(--background)" strokeWidth={2} />
            ))}
          </g>
        ))}
      </svg>
      {tip}
    </div>
  )
}

/** the same numbers as a table (每个图可切“表格”) */
export function ChartTable({ data, fmt, picked, onPick }: { data: Series; fmt: (v: number) => string; picked?: string | null; onPick?: (d: string | null) => void }) {
  return (
    <div className="overflow-x-auto" data-chart-table>
      <table className="w-full type-footnote tabular-nums">
        <thead><tr className="text-muted-foreground"><th className="py-1.5 pr-3 text-left font-medium">日期</th>{data.series.map((l) => <th key={l.key} className="py-1.5 pl-3 text-right font-medium">{l.label}</th>)}</tr></thead>
        <tbody>{data.buckets.map((d, i) => (
          <tr key={d} className={cn("cursor-pointer border-t hover:bg-accent", picked === d && "bg-accent")} onClick={() => onPick?.(picked === d ? null : d)}>
            <td className="py-1.5 pr-3">{d}</td>{data.series.map((l) => <td key={l.key} className="py-1.5 pl-3 text-right">{l.values[i] == null ? "—" : fmt(l.values[i]!)}</td>)}
          </tr>))}</tbody>
      </table>
    </div>
  )
}

/** a thin share bar in a table cell (4px, grey) */
export function Share({ r }: { r: number }) {
  return <span className="block h-1 w-full min-w-10 overflow-hidden rounded-full bg-muted" aria-hidden><span className="block h-full rounded-full bg-foreground/40" style={{ width: `${Math.max(0, Math.min(1, r)) * 100}%` }} /></span>
}

/** a few tiny columns for a flow figure (迷你柱) */
export function Spark({ values }: { values: number[] }) {
  const m = Math.max(1, ...values)
  return (
    <span className="inline-flex h-5 items-end gap-[2px]" aria-hidden>
      {values.map((v, i) => <span key={i} className={cn("w-[3px] rounded-t-[1px]", i === values.length - 1 ? "bg-foreground/60" : "bg-foreground/20")} style={{ height: `${Math.max(1, (v / m) * 20)}px` }} />)}
    </span>
  )
}
