/* 状态与报告面板：统计、设备状态、YOLO 记录、单件事项报告共用的一套卡片（visual system v2）。
 * 一张卡片讲一件事：一层浮起的面（bg-card + --lift），不再用描边框和框中框。从上到下：
 *   头部   柔和的图标块 + 标题（type-headline）+ 一行说明；右边一个状态签（运行中 / 实时 / 草稿）或这张卡片自己的切换
 *   正文   一个或几个分区；头部和第一个分区之间没有线，之后的分区之间一条细线；分区的头是小标签和右边的范围
 *   数字条 几个数字并排，没有竖线：标签在上，数字在下（正文字体、等宽数位 tabular-nums）；只在要提醒时着色
 *   脚注   左边的时间或说明，右边的按钮（次要在前，主要在后）
 * 等宽字只留给编号和代码；数字用 tabular-nums 对齐。颜色只留给状态和分组，其余都是灰阶。 */
import { Children, type ReactNode } from "react"
import { cn } from "cn"

/** the square icon tile at the head of a panel */
export const PanelIcon = ({ children, className }: { children: ReactNode; className?: string }) => (
  <span className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground [&_svg]:size-4 [&_svg]:stroke-[1.75]", className)} aria-hidden>{children}</span>
)

export type Tone = "run" | "ok" | "wait" | "bad" | "muted"
const TONE: Record<Tone, string> = {
  run: "bg-blue/10 text-blue",
  ok: "bg-success/12 text-success",
  wait: "bg-attention/12 text-attention",
  bad: "bg-destructive/10 text-destructive",
  muted: "bg-muted text-muted-foreground",
}
/** a small status tag (“运行中”, “草稿”, “Hub”) */
export const Tag = ({ tone = "muted", children, className, ...rest }: { tone?: Tone; children: ReactNode; className?: string; [k: string]: unknown }) => (
  <span className={cn("inline-flex h-6 shrink-0 items-center gap-1.5 rounded-md px-2 type-label whitespace-nowrap", TONE[tone], className)} {...rest}>{children}</span>
)
const VALUE: Record<Tone, string> = { run: "text-blue", ok: "text-success", wait: "text-attention", bad: "text-destructive", muted: "text-muted-foreground" }

/** a card: head (icon, title, one grey line, a tag or switch on the right), then its sections, strip and foot */
export function Panel({ icon, title, sub, right, children, className, k, ...rest }: {
  icon?: ReactNode; title?: ReactNode; sub?: ReactNode; right?: ReactNode; children?: ReactNode; className?: string; k?: string; [x: string]: unknown
}) {
  return (
    <section className={cn("flex min-w-0 flex-col overflow-hidden rounded-xl bg-card text-card-foreground shadow-[var(--lift)]", className)} data-panel={k} {...rest}>
      {title != null && (
        <header className={cn("flex flex-wrap items-center gap-x-3 gap-y-3 px-5 pt-4", Children.toArray(children).length ? "pb-2" : "pb-4")}>
          {icon && <PanelIcon>{icon}</PanelIcon>}
          <div className="min-w-0 flex-1 basis-40">
            <h2 className="truncate type-headline text-foreground">{title}</h2>
            {sub && <div className="type-footnote text-pretty text-muted-foreground">{sub}</div>}
          </div>
          {right && <div className="flex shrink-0 flex-wrap items-center justify-end gap-2">{right}</div>}
        </header>
      )}
      {/* sections are set apart by space, not rules */}
      <div className="flex flex-1 flex-col">{children}</div>
    </section>
  )
}

/** a section of a panel: a grey label left, a monospace scope right, then its content */
export function PanelSection({ label, meta, right, children, className, flush, k }: { label?: ReactNode; meta?: ReactNode; right?: ReactNode; children?: ReactNode; className?: string; flush?: boolean; k?: string }) {
  return (
    <div className={cn(flush ? "py-3" : "px-5 py-4", className)} data-panel-section={k}>
      {(label != null || meta != null || right != null) && (
        <div className={cn("mb-3 flex min-h-5 flex-wrap items-center gap-x-3 gap-y-1", flush && "px-5")}>
          {label != null && <span className="min-w-0 flex-auto type-label text-tertiary">{label}</span>}
          {meta != null && <span className="min-w-0 type-footnote tabular-nums text-muted-foreground">{meta}</span>}
          {right}
        </div>
      )}
      {children}
    </div>
  )
}

export type Stat = { label: ReactNode; value: ReactNode; sub?: ReactNode; tone?: Tone; k?: string; title?: string }
/** figures side by side: a grey label over a figure, set apart by space rather than lines */
export function StatStrip({ items, cols, className }: { items: (Stat | null | false | undefined)[]; cols?: number; className?: string }) {
  const xs = items.filter(Boolean) as Stat[]
  const n = cols ?? xs.length
  return (
    <div className={cn(STRIP, n >= 4 ? "grid-cols-2 sm:grid-cols-4" : n === 3 ? "grid-cols-3" : n === 2 ? "grid-cols-2" : "grid-cols-1", className)} data-stat-strip>
      {xs.map((s, i) => (
        <div key={s.k ?? i} title={s.title} className={STAT_CELL} data-stat={s.k}>
          <div className="truncate type-label text-tertiary">{s.label}</div>
          <div className={cn(STAT_VALUE, s.tone ? VALUE[s.tone] : "text-foreground")}>{s.value}</div>
          {s.sub != null && <div className="line-clamp-2 type-footnote text-tertiary">{s.sub}</div>}
        </div>
      ))}
    </div>
  )
}

/** a strip of figures (here and in the stats KPIs): cells with their own padding, no rules between them */
export const STRIP = "grid gap-x-2 px-2 py-2"
export const STAT_CELL = "min-w-0 rounded-lg px-3 py-2.5"
/** a figure: the body face at title size, digits of one width so figures side by side line up */
export const STAT_VALUE = "mt-0.5 truncate type-title font-semibold tabular-nums"

/** the last line: grey context left, buttons right */
export function PanelFoot({ meta, children, className }: { meta?: ReactNode; children?: ReactNode; className?: string }) {
  return (
    <div className={cn("flex min-h-14 flex-wrap items-center gap-x-3 gap-y-2 px-5 py-3", className)} data-panel-foot>
      <div className="min-w-0 flex-1 type-footnote text-muted-foreground">{meta}</div>
      {Children.count(children) > 0 && <div className="flex shrink-0 flex-wrap items-center gap-2">{children}</div>}
    </div>
  )
}

/** a thin meter (4px): how full, coloured only when it needs attention */
export function Meter({ value, tone, className }: { value: number | null | undefined; tone?: Tone; className?: string }) {
  const v = Math.max(0, Math.min(1, value ?? 0))
  return (
    <span className={cn("block h-1 w-full overflow-hidden rounded-full bg-muted", className)} aria-hidden>
      <span className={cn("block h-full rounded-full transition-[width] duration-260 ease-(--ease-out)", tone === "bad" ? "bg-destructive" : tone === "wait" ? "bg-attention" : tone === "ok" ? "bg-success" : tone === "run" ? "bg-blue" : "bg-foreground/70")} style={{ width: `${v * 100}%` }} />
    </span>
  )
}

/** a series reduced to about `n` points: each bucket is the mean of the values in it (gaps stay gaps), so a day of
 *  per-minute samples reads as a trend rather than a barcode */
export function downsample(values: (number | null | undefined)[], n = 48): (number | null)[] {
  if (values.length <= n) return values.map((v) => v ?? null)
  const out: (number | null)[] = []
  for (let b = 0; b < n; b++) {
    const xs = values.slice(Math.floor((b * values.length) / n), Math.floor(((b + 1) * values.length) / n)).filter((v): v is number => v != null)
    out.push(xs.length ? xs.reduce((a, v) => a + v, 0) / xs.length : null)
  }
  return out
}

/** a small line of a series (values 0..1, gaps break the line) over a faint area; without a series, an empty box of the
 *  same size, so the rows' figures stay in one column */
export function Spark({ values, label, className, color = "currentColor" }: { values: (number | null | undefined)[]; label: string; className?: string; color?: string }) {
  const xs = downsample(values)
  if (xs.filter((v) => v != null).length < 2) return <span className={cn("h-6 w-20 shrink-0", className)} aria-hidden />
  const n = Math.max(1, xs.length - 1)
  const pts = xs.map((v, i) => (v == null ? null : ([(i / n) * 100, 22 - Math.min(1, Math.max(0, v)) * 20] as const)))
  /* runs of consecutive points: each is one stroke and one filled area down to the baseline */
  const runs: (readonly [number, number])[][] = []
  pts.forEach((p, i) => { if (!p) return; if (i && pts[i - 1]) runs[runs.length - 1].push(p); else runs.push([p]) })
  const line = runs.map((r) => r.map((p, k) => (k ? "L" : "M") + p[0].toFixed(1) + " " + p[1].toFixed(1)).join("")).join("")
  const area = runs.filter((r) => r.length > 1).map((r) => r.map((p, k) => (k ? "L" : "M") + p[0].toFixed(1) + " " + p[1].toFixed(1)).join("") + `L${r[r.length - 1][0].toFixed(1)} 24L${r[0][0].toFixed(1)} 24Z`).join("")
  return (
    <svg viewBox="0 0 100 24" preserveAspectRatio="none" className={cn("h-6 w-20 shrink-0 text-tertiary", className)} role="img" aria-label={label}>
      <path d={area} fill={color} fillOpacity={0.1} stroke="none" />
      <path d={line} fill="none" stroke={color} strokeWidth="1.25" strokeLinejoin="round" strokeLinecap="round" vectorEffect="non-scaling-stroke" />
    </svg>
  )
}

/** a list row inside a panel: name and grey sub on the left, anything on the right (a spark, a figure with its meter) */
/** a row of a panel; with `onClick` the row opens something. `controls`: its children are buttons of their own, so the row
 *  is a plain box under a full-size open button instead of one button around them (no button inside a button) */
export function PanelRow({ title, sub, children, onClick, className, controls = false, ...rest }: { title: ReactNode; sub?: ReactNode; children?: ReactNode; onClick?: () => void; className?: string; controls?: boolean; [k: string]: unknown }) {
  const body = <>
    <span className="min-w-0 flex-1">
      <span className="block truncate type-body text-foreground">{title}</span>
      {sub != null && <span className="block truncate type-footnote text-muted-foreground">{sub}</span>}
    </span>
    {children}
  </>
  /* rows sit inset from the panel's edge, so a hover fills a rounded row rather than a stripe of the card */
  const cls = cn("relative mx-2 flex w-[calc(100%-1rem)] min-w-0 items-center gap-4 rounded-lg px-3 py-2.5 text-left [&+&]:before:absolute [&+&]:before:inset-x-3 [&+&]:before:top-0 [&+&]:before:h-px [&+&]:before:bg-border/60", onClick && "cursor-pointer hover:bg-muted/60 hover:before:opacity-0 [&:hover+&]:before:opacity-0 focus-visible:bg-muted/60 focus-visible:outline-none", className)
  if (onClick && controls) return (
    <div className={cn(cls, "has-[[data-row-cover]:focus-visible]:bg-muted/60")} {...rest}>
      <button type="button" onClick={onClick} aria-label={typeof title === "string" ? title : undefined} className="absolute inset-0 cursor-pointer rounded-lg outline-none" data-row-cover />
      <span className="pointer-events-none relative min-w-0 flex-1">
        <span className="block truncate type-body text-foreground">{title}</span>
        {sub != null && <span className="block truncate type-footnote text-muted-foreground">{sub}</span>}
      </span>
      <span className="relative flex shrink-0 items-center gap-4">{children}</span>
    </div>)
  return onClick ? <button type="button" onClick={onClick} className={cls} {...rest}>{body}</button> : <div className={cls} {...rest}>{body}</div>
}

/** a figure with its meter under it, right-aligned (the “FP rate” column) */
export function MeterValue({ value, ratio, tone, className }: { value: ReactNode; ratio: number | null | undefined; tone?: Tone; className?: string }) {
  return (
    <span className={cn("flex w-16 shrink-0 flex-col items-end gap-1.5", className)}>
      <span className="type-callout font-medium tabular-nums text-foreground">{value}</span>
      <Meter value={ratio} tone={tone} className="w-12" />
    </span>
  )
}
