/* 统计图（stats.md §6.2–6.3，ST-40 重整）：单一纵轴；颜色分两种用途，整页一致——
 * 状态（新建、进行中、等你、完成、失败……）用 STATE，和事项列表的状态点同色；“谁”（模型、Bot、项目、token 类型）用 SERIES，
 * 按顺序取，第 6 个起并入“其他”。堆叠柱 / 并列柱（按天）、折线（命中率）、中位数折线带 p85 浅带（耗时）、横向条形（分布）。
 * 十字线 + 提示框，键盘左右键同样能看；点一天回调；每个图可切表格。
 * 按小时（ST-43，单日范围）：横轴是统计时区的 00:00–23:00；当前小时垫一层浅底、柱子变浅、标“未结束”；
 * 之后的小时 Hub 给 null，留空（不画 0），提示框写“还没到”，也不能点。 */
import { useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react"
import { cn } from "cn"
import type { Series } from "@/api/stats"
import { t, useT } from "@/i18n"
import { bucketName, bucketTick, hhmm, hourSpan } from "@/stats/format"
import { PanelIcon } from "@/panel"

/** identity colours, in order (index.css `--series-*`, validated light and dark) */
export const SERIES = [1, 2, 3, 4, 5].map((n) => `var(--series-${n})`)
export const OTHER = "var(--series-other)"
/** state colours: one state, one colour, on every chart of the page and as its status dot */
export const STATE = {
  created: "color-mix(in oklab, var(--foreground) 45%, var(--background))",
  run: "var(--blue)", wait: "var(--attention)", done: "var(--success)", fail: "var(--destructive)",
  idle: "var(--tertiary)", band: "color-mix(in oklab, var(--foreground) 10%, var(--background))",
} as const
export const seriesColor = (i: number, key?: string) => (key === "other" ? OTHER : SERIES[i] ?? OTHER)

/** a toolbar control (a Select, a date button) in the page's one control language: the muted fill of a Segmented, no outline */
export const TOOL = "border-transparent bg-muted font-normal text-foreground hover:bg-accent"

/** a legend key: a dot for bars and areas, a short stroke for lines, a tinted dot ringed in its colour for a band */
export const Swatch = ({ c, line, band }: { c: string; line?: boolean; band?: boolean }) => <span aria-hidden className={cn("inline-block shrink-0 rounded-full", line ? "h-0.5 w-3" : band ? "size-2.5" : "size-2")}
  style={band ? { background: `color-mix(in oklab, ${c} 35%, transparent)`, boxShadow: `inset 0 0 0 1px ${c}` } : { background: c }} />
/** the fill of a band (median to p85) in its line's colour */
export const bandFill = (c: string) => `color-mix(in oklab, ${c} 16%, transparent)`
export type LegendEntry = { label: ReactNode; c: string; line?: boolean; band?: boolean }
export function Legend({ items, className }: { items: LegendEntry[]; className?: string }) {
  return (
    <div className={cn("flex flex-wrap gap-x-4 gap-y-1 type-footnote text-muted-foreground", className)} data-legend>
      {items.map((x, i) => <span key={i} className="inline-flex min-w-0 items-center gap-1.5"><Swatch c={x.c} line={x.line} band={x.band} /><span className="truncate">{x.label}</span></span>)}
    </div>
  )
}

/** a chart panel's head (stats.md §6.3, panel.tsx): icon, title and the chart's own switches; under them the legend on the
 *  left and “单位 · 时间范围” on the right. The chart that follows goes in a `ChartBody`. */
export function ChartHead({ title, meta, legend, right, k, icon, sub }: { title: ReactNode; meta?: ReactNode; legend?: LegendEntry[]; right?: ReactNode; k?: string; icon?: ReactNode; sub?: ReactNode }) {
  return (
    <div data-chart-head={k}>
      <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-2 px-5 pt-4 pb-2">
        <div className="flex min-w-0 items-center gap-3">
          {icon && <PanelIcon>{icon}</PanelIcon>}
          <div className="min-w-0">
            <h3 className="truncate type-headline text-foreground">{title}</h3>
            {sub && <div className="type-footnote text-pretty text-muted-foreground">{sub}</div>}
          </div>
        </div>
        {right && <div className="flex flex-wrap items-center gap-2">{right}</div>}
      </div>
      {(meta || (legend && legend.length > 0)) && (
        <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1.5 px-5 pt-2">
          {legend && legend.length > 0 ? <Legend items={legend} /> : <span />}
          {meta && <div className="type-footnote tabular-nums text-tertiary" data-chart-meta>{meta}</div>}
        </div>
      )}
    </div>
  )
}
/** the chart under a `ChartHead`, in the same section (no hairline between them) */
export const ChartBody = ({ children, className }: { children: ReactNode; className?: string }) => <div className={cn("flex-1 border-t-0! px-5 pt-3 pb-4", className)}>{children}</div>

/** at most 5 identity series; the rest summed into “其他” (stacks only: a rate can't be summed) */
export function fold(d: Series, max = 5): Series {
  if (d.series.length <= max) return d
  const keep = d.series.slice(0, max - 1), rest = d.series.slice(max - 1)
  const other = { key: "other", label: t("stats:chart.otherN", { count: rest.length }), values: d.buckets.map((_, i) => rest.reduce<number | null>((s, l) => (l.values[i] == null ? s : (s ?? 0) + l.values[i]!), null)) }
  return { ...d, series: [...keep, other] }
}

export type Kind = "stack" | "group" | "line" | "band"
const H = 220, PAD = { l: 48, r: 12, t: 12, b: 26 }

/** the top of the axis: 1 / 2 / 2.5 / 5 × 10ⁿ; counts get an even whole number so the middle tick is whole too */
function niceMax(v: number, int = false): number {
  if (int) return v <= 10 ? Math.max(2, Math.ceil(v / 2) * 2) : Math.ceil(niceMax(v) / 2) * 2
  if (v <= 0) return 1
  const p = 10 ** Math.floor(Math.log10(v)), m = v / p
  return (m <= 1 ? 1 : m <= 2 ? 2 : m <= 2.5 ? 2.5 : m <= 5 ? 5 : 10) * p
}
/** a bar with 4px rounded data end, anchored on the baseline */
const bar = (xl: number, w: number, y0: number, h: number, r: number) => {
  const yt = y0 - h
  return `M${xl},${y0}V${yt + r}Q${xl},${yt} ${xl + r},${yt}H${xl + w - r}Q${xl + w},${yt} ${xl + w},${yt + r}V${y0}Z`
}

export function Chart({ data, kind, fmt, picked, onPick, label, colors, height = H, int = false, unit = 1, tipFmt, xFmt = bucketTick }: {
  data: Series; kind: Kind; fmt: (v: number) => string; picked?: string | null; onPick?: (day: string | null) => void; label: string
  /** one colour per series (defaults to the identity order) */
  colors?: string[]; height?: number
  /** whole numbers (counts): whole ticks; `unit`: the axis' unit in data units (3600000 for hours), so its ticks are round in it */
  int?: boolean; unit?: number
  /** the tooltip's format, when it should be finer than the axis */
  tipFmt?: (v: number) => string
  /** a bucket as the axis writes it (days by default) */
  xFmt?: (bucket: string) => string
}) {
  const t = useT()
  const box = useRef<HTMLDivElement>(null)
  const [w, setW] = useState(640)
  const [hover, setHover] = useState<number | null>(null)
  /* measured before the first paint, so a chart switched back from its table never shows at the default width */
  useLayoutEffect(() => {
    const el = box.current; if (!el) return
    const ro = new ResizeObserver(() => setW(el.clientWidth || 640)); ro.observe(el); setW(el.clientWidth || 640)
    return () => ro.disconnect()
  }, [])
  const n = data.buckets.length, lines = data.series
  const color = (k: number) => colors?.[k] ?? seriesColor(k, lines[k]?.key)
  const iw = Math.max(1, w - PAD.l - PAD.r), ih = height - PAD.t - PAD.b, slot = iw / Math.max(1, n)
  const stackTop = useMemo(() => data.buckets.map((_, i) => lines.reduce((s, l) => s + (l.values[i] ?? 0), 0)), [data, lines])
  const max = niceMax((kind === "stack" ? Math.max(0, ...stackTop) : Math.max(0, ...lines.flatMap((l) => l.values.map((v) => v ?? 0)))) / unit, int) * unit
  const tf = tipFmt ?? fmt
  const y = (v: number) => PAD.t + ih - (v / max) * ih
  const x = (i: number) => PAD.l + slot * i + slot / 2
  const ticks = [0, 0.5, 1].map((k) => k * max)
  const hourly = data.bucket === "hour", cur = hourly ? data.current ?? -1 : -1
  /** an hour after the current one: not yet */
  const future = (i: number) => cur >= 0 && i > cur
  const every0 = Math.max(1, Math.ceil(n / Math.max(2, Math.floor(iw / 56))))
  const every = hourly ? [1, 2, 3, 4, 6, 12].find((k) => k >= every0) ?? 12 : every0
  const bw = Math.min(kind === "group" ? 28 : 24, Math.max(3, slot * 0.62))
  const gw = kind === "group" ? Math.max(2, (bw - 2 * (lines.length - 1)) / Math.max(1, lines.length)) : bw
  const at = hover ?? (picked ? data.buckets.indexOf(picked) : -1)
  const dim = (i: number) => (at >= 0 && i !== at ? 0.45 : 1) * (i === cur ? 0.6 : 1)
  /* a band chart joins the days that have a value (as its band does); a plain line breaks at a missing day */
  const path = (vals: (number | null)[]) => {
    let d = "", pen = false
    vals.forEach((v, i) => { if (v == null) { if (kind !== "band") pen = false; return } d += (pen ? "L" : "M") + x(i).toFixed(1) + "," + y(v).toFixed(1); pen = true })
    return d
  }
  const lone = (vals: (number | null)[], i: number) => (i === 0 || vals[i - 1] == null) && (i === vals.length - 1 || vals[i + 1] == null)
  const key = (e: KeyboardEvent) => {
    if (!n) return
    const right = e.key === "ArrowRight"
    if (right || e.key === "ArrowLeft") { e.preventDefault(); setHover(at < 0 ? (right ? 0 : n - 1) : Math.min(n - 1, Math.max(0, at + (right ? 1 : -1)))) }
    else if ((e.key === "Enter" || e.key === " ") && at >= 0 && !future(at)) { e.preventDefault(); onPick?.(data.buckets[at] !== picked ? data.buckets[at] : null) }
    else if (e.key === "Escape") setHover(null)
  }
  const tip: ReactNode = at >= 0 && at < n && (
    <div role="status" className="pointer-events-none absolute top-1 z-10 min-w-36 max-w-60 rounded-lg bg-popover px-3 py-2 text-popover-foreground shadow-md transition-[left] duration-120 ease-(--ease-out)"
      style={{ left: Math.min(Math.max(0, x(at) + 12), Math.max(0, w - 200)) }} data-chart-tip>
      <div className="type-label text-muted-foreground">{bucketName(data.buckets[at])}</div>
      {at === cur && <div className="mt-0.5 type-footnote text-tertiary" data-tip-current>{t("stats:chart.openUntil", { time: hhmm(Date.now()) })}</div>}
      {future(at) ? <div className="mt-1 type-footnote text-muted-foreground" data-tip-future>{t("stats:chart.notYet")}</div> : lines.map((l, i) => (kind !== "band" || i === 0 || lines.length < 2) ? (
        <div key={l.key} className="mt-1 flex items-center gap-2 type-footnote"><Swatch c={color(i)} line={kind === "line" || kind === "band"} /><span className="min-w-0 flex-1 truncate text-muted-foreground">{l.label}</span><span className="font-medium tabular-nums text-foreground">{l.values[at] == null ? "—" : tf(l.values[at]!)}</span></div>
      ) : (
        <div key={l.key} className="mt-1 flex items-center gap-2 type-footnote"><Swatch c={color(0)} band /><span className="min-w-0 flex-1 truncate text-muted-foreground">{l.label}</span><span className="font-medium tabular-nums text-foreground">{l.values[at] == null ? "—" : tf(l.values[at]!)}</span></div>
      ))}
      {kind === "stack" && lines.length > 1 && !future(at) && <div className="mt-1 flex justify-between border-t pt-1 type-footnote"><span className="text-muted-foreground">{t("stats:chart.total")}</span><span className="font-medium tabular-nums">{tf(stackTop[at])}</span></div>}
      {onPick && !future(at) && <div className="mt-1 type-footnote text-tertiary">{data.buckets[at] === picked ? t("stats:chart.clickCancel") : hourly ? t("stats:chart.clickHour") : t("stats:chart.clickDay")}</div>}
    </div>
  )
  return (
    <div ref={box} className="anim-in relative w-full select-none" data-chart={kind}>
      <svg width={w} height={height} role="img" aria-label={label} tabIndex={0} onKeyDown={key} onBlur={() => setHover(null)}
        className={cn("block overflow-visible rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring", onPick && "cursor-pointer")}
        onMouseLeave={() => setHover(null)}
        onMouseMove={(e) => { const r = e.currentTarget.getBoundingClientRect(); const i = Math.floor((e.clientX - r.left - PAD.l) / slot); setHover(i >= 0 && i < n ? i : null) }}
        onClick={() => { if (hover == null || !future(hover)) onPick?.(hover != null && data.buckets[hover] !== picked ? data.buckets[hover] : null) }}>
        {cur >= 0 && <g data-current={data.buckets[cur]}>
          <rect x={PAD.l + slot * cur} y={PAD.t} width={slot} height={ih} fill="var(--muted)" opacity={0.8} />
          <text x={x(cur)} y={PAD.t - 3} textAnchor="middle" className="fill-tertiary type-footnote">{t("stats:chart.open")}</text>
        </g>}
        {ticks.map((v) => (
          <g key={v}>
            <line x1={PAD.l} x2={w - PAD.r} y1={y(v)} y2={y(v)} stroke="var(--border)" strokeWidth={1} strokeOpacity={v ? 0.6 : 1} />
            <text x={PAD.l - 10} y={y(v)} dy="0.32em" textAnchor="end" className="fill-tertiary type-footnote tabular-nums">{fmt(v)}</text>
          </g>
        ))}
        {data.buckets.map((d, i) => i % every === 0 && <text key={d} x={x(i)} y={height - 8} textAnchor="middle" className={cn("type-footnote tabular-nums", d === picked || i === cur ? "fill-foreground font-medium" : future(i) ? "fill-tertiary" : "fill-muted-foreground")}>{xFmt(d)}</text>)}
        {at >= 0 && at < n && kind !== "stack" && kind !== "group" && <line x1={x(at)} x2={x(at)} y1={PAD.t} y2={PAD.t + ih} stroke="var(--border)" strokeWidth={1} />}
        {kind === "stack" && data.buckets.map((d, i) => {
          let base = 0
          return (
            <g key={d} data-bar={d} opacity={dim(i)} className="transition-opacity duration-120">
              {lines.map((l, k) => {
                const v = l.values[i] ?? 0
                if (v <= 0) return null
                const y0 = y(base); base += v
                const top = lines.slice(k + 1).every((m) => (m.values[i] ?? 0) <= 0)
                const h = Math.max(1, y0 - y(base) - (top ? 0 : 2)) // a 2px surface gap between segments
                return <path key={l.key} fill={color(k)} d={bar(x(i) - bw / 2, bw, y0, h, top ? Math.min(4, h / 2, bw / 2) : 0)} />
              })}
            </g>
          )
        })}
        {kind === "group" && data.buckets.map((d, i) => (
          <g key={d} data-bar={d} opacity={dim(i)} className="transition-opacity duration-120">
            {lines.map((l, k) => {
              const v = l.values[i] ?? 0
              if (v <= 0) return null
              const h = Math.max(1, y(0) - y(v))
              return <path key={l.key} fill={color(k)} d={bar(x(i) - bw / 2 + k * (gw + 2), gw, y(0), h, Math.min(4, h / 2, gw / 2))} />
            })}
          </g>
        ))}
        {kind === "band" && lines.length >= 2 && (() => {
          const lo = lines[0].values, hi = lines[1].values
          const pts = data.buckets.map((_, i) => i).filter((i) => lo[i] != null && hi[i] != null)
          if (!pts.length) return null
          const d = pts.map((i, k) => (k ? "L" : "M") + x(i) + "," + y(hi[i]!)).join("") + pts.slice().reverse().map((i) => "L" + x(i) + "," + y(lo[i]!)).join("") + "Z"
          return <path d={d} fill={bandFill(color(0))} />
        })()}
        {(kind === "line" || kind === "band") && lines.slice(0, kind === "band" ? 1 : undefined).map((l, k) => (
          <g key={l.key}>
            <path d={path(l.values)} fill="none" stroke={color(k)} strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
            {l.values.map((v, i) => v != null && (i === at || kind === "band" || lone(l.values, i)) && (
              <circle key={i} cx={x(i)} cy={y(v)} r={i === at ? 4.5 : 3.5} fill={color(k)} stroke="var(--background)" strokeWidth={2} />
            ))}
          </g>
        ))}
      </svg>
      {tip}
    </div>
  )
}

/** the same numbers as a table (每个图可切“表格”) */
export function ChartTable({ data, fmt, picked, onPick, xFmt = bucketTick, height }: { data: Series; fmt: (v: number) => string; picked?: string | null; onPick?: (d: string | null) => void; xFmt?: (bucket: string) => string
  /** the height of the chart it stands in for: the table scrolls inside it, so switching moves nothing around it */
  height?: number }) {
  const t = useT()
  const hourly = data.bucket === "hour", cur = hourly ? data.current ?? -1 : -1
  return (
    <div className="anim-in overflow-auto overscroll-contain pb-4 [mask-image:linear-gradient(to_bottom,black_calc(100%-20px),transparent)]" style={{ height: height ?? undefined, maxHeight: height ? undefined : 320 }} data-chart-table>
      <table className="w-full type-footnote tabular-nums">
        <thead className="sticky top-0 bg-card"><tr className="text-muted-foreground [&>th]:border-b"><th className="py-1.5 pr-3 text-left font-medium">{hourly ? t("stats:col.hourSpan") : t("stats:col.date")}</th>{data.series.map((l) => <th key={l.key} className="py-1.5 pl-3 text-right font-medium">{l.label}</th>)}</tr></thead>
        <tbody>{data.buckets.map((d, i) => {
          const later = cur >= 0 && i > cur, can = onPick && !later
          return (
            <tr key={d} className={cn("[&+&]:border-t [&+&]:border-border/60", can && "cursor-pointer hover:bg-accent", picked === d && "bg-accent", later && "text-tertiary")} onClick={() => can && onPick(picked === d ? null : d)} data-table-row={d}>
              <td className="py-1.5 pr-3 tabular-nums" title={hourly ? undefined : d}>{hourly ? hourSpan(d) : xFmt(d)}{i === cur && <span className="ml-1 text-tertiary">{t("stats:chart.open")}</span>}</td>
              {data.series.map((l) => <td key={l.key} className="py-1.5 pl-3 text-right">{later ? t("stats:chart.notCome") : l.values[i] == null ? "—" : fmt(l.values[i]!)}</td>)}
            </tr>)
        })}</tbody>
      </table>
    </div>
  )
}

export type BarRow = { key: string; label: ReactNode; value: number; c: string; sub?: ReactNode; title?: string }
/** a distribution as horizontal bars: label, bar, value — every bar is labelled, so colour is never the only key.
 *  `onPick` makes each row a button (下钻); `total` sets the 100% (defaults to the largest bar). */
export function HBars({ rows, fmt = String, onPick, picked, label, total, k, vw = "w-14", lw = "w-[5.5rem] sm:w-32" }: {
  rows: BarRow[]; fmt?: (v: number) => string; onPick?: (key: string) => void; picked?: string | null; label: string; total?: number; k?: string
  /** the width of the value column */
  vw?: string
  /** the width of the label column */
  lw?: string
}) {
  const max = Math.max(1, total ?? Math.max(0, ...rows.map((r) => r.value)))
  return (
    <div role={onPick ? "group" : "list"} aria-label={label} className="flex flex-col" data-hbars={k}>
      {rows.map((r) => {
        const body = <>
          <span className={cn(lw, "shrink-0 truncate text-left type-callout text-foreground/90")} title={r.title}>{r.label}</span>
          <span className="relative h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-muted" aria-hidden>
            {r.value > 0 && <span className="absolute inset-y-0 left-0 rounded-full transition-[width] duration-260 ease-(--ease-out)" style={{ width: `${Math.max(1.5, (r.value / max) * 100)}%`, background: r.c }} />}
          </span>
          <span className={cn(vw, "shrink-0 text-right")}>
            <span className="block type-callout font-medium tabular-nums">{fmt(r.value)}</span>
            {r.sub != null && <span className="block truncate type-footnote tabular-nums text-tertiary">{r.sub}</span>}
          </span>
        </>
        const cls = "flex w-full items-center gap-3 rounded-lg px-1.5 py-1.5"
        return onPick
          ? <button key={r.key} type="button" onClick={() => onPick(r.key)} aria-pressed={picked === r.key} className={cn(cls, "text-left transition-colors hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:outline-none", picked === r.key && "bg-muted")} data-hbar={r.key}>{body}</button>
          : <div key={r.key} role="listitem" className={cls} data-hbar={r.key}>{body}</div>
      })}
    </div>
  )
}

/** a thin share bar in a table cell (4px, grey) */
export function Share({ r }: { r: number }) {
  return <span className="block h-1 w-full min-w-10 overflow-hidden rounded-full bg-muted" aria-hidden><span className="block h-full rounded-full bg-foreground/35 transition-[width] duration-260 ease-(--ease-out)" style={{ width: `${Math.max(0, Math.min(1, r)) * 100}%` }} /></span>
}
