/* 状态与报告面板：统计、设备状态、YOLO 记录、单件事项报告共用的一套卡片。
 * 一张卡片讲一件事，从上到下固定四层，层与层之间是一条细线：
 *   头部   图标方块 + 标题 + 一行灰色说明；右边一个状态签（运行中 / 实时 / 草稿）或这张卡片自己的切换
 *   正文   一个或几个分区；每个分区的头是左边灰色小标签、右边等宽字的范围或编号（“近 7 天”“Run 1288”）
 *   数字条 三四个数字并排，竖线隔开：灰色标签在上，等宽数字在下；只在要提醒时着色
 *   脚注   左边灰色的时间或说明，右边的按钮（次要在前，主要在后）
 * 数字、时间、编号一律等宽字（font-mono tabular-nums），文字用正文字体；颜色只留给状态和分组，其余都是灰阶。 */
import { Children, type ReactNode } from "react"
import { cn } from "cn"

/** the square icon tile at the head of a panel */
export const PanelIcon = ({ children, className }: { children: ReactNode; className?: string }) => (
  <span className={cn("flex size-9 shrink-0 items-center justify-center rounded-[10px] border bg-background text-foreground/80 [&_svg]:size-4 [&_svg]:stroke-[1.75]", className)} aria-hidden>{children}</span>
)

export type Tone = "run" | "ok" | "wait" | "bad" | "muted"
const TONE: Record<Tone, string> = {
  run: "border-blue/25 bg-blue/10 text-blue",
  ok: "border-success/25 bg-success/10 text-success",
  wait: "border-attention/25 bg-attention/10 text-attention",
  bad: "border-destructive/25 bg-destructive/10 text-destructive",
  muted: "border-border bg-muted/50 text-muted-foreground",
}
/** a small status tag (“运行中”, “草稿”, “Hub”) */
export const Tag = ({ tone = "muted", children, className, ...rest }: { tone?: Tone; children: ReactNode; className?: string; [k: string]: unknown }) => (
  <span className={cn("inline-flex h-6 shrink-0 items-center gap-1.5 rounded-md border px-2 text-[12px] font-medium leading-none", TONE[tone], className)} {...rest}>{children}</span>
)
const VALUE: Record<Tone, string> = { run: "text-blue", ok: "text-success", wait: "text-attention", bad: "text-destructive", muted: "text-muted-foreground" }

/** a card: head (icon, title, one grey line, a tag or switch on the right), then its sections, strip and foot */
export function Panel({ icon, title, sub, right, children, className, k, ...rest }: {
  icon?: ReactNode; title?: ReactNode; sub?: ReactNode; right?: ReactNode; children?: ReactNode; className?: string; k?: string; [x: string]: unknown
}) {
  return (
    <section className={cn("flex min-w-0 flex-col overflow-hidden rounded-2xl border bg-card text-card-foreground", className)} data-panel={k} {...rest}>
      {title != null && (
        <header className="flex flex-wrap items-center gap-x-3 gap-y-3 px-5 py-4">
          {icon && <PanelIcon>{icon}</PanelIcon>}
          <div className="min-w-0 flex-1 basis-40">
            <h2 className="truncate text-[15px] font-medium leading-5 text-foreground">{title}</h2>
            {sub && <div className="mt-0.5 line-clamp-2 type-footnote text-muted-foreground">{sub}</div>}
          </div>
          {right && <div className="flex shrink-0 flex-wrap items-center justify-end gap-2">{right}</div>}
        </header>
      )}
      <div className={cn("flex flex-1 flex-col [&>*]:border-t", title == null && "[&>*:first-child]:border-t-0")}>{children}</div>
    </section>
  )
}

/** a section of a panel: a grey label left, a monospace scope right, then its content */
export function PanelSection({ label, meta, right, children, className, flush, k }: { label?: ReactNode; meta?: ReactNode; right?: ReactNode; children?: ReactNode; className?: string; flush?: boolean; k?: string }) {
  return (
    <div className={cn(flush ? "py-3" : "px-5 py-4", className)} data-panel-section={k}>
      {(label != null || meta != null || right != null) && (
        <div className={cn("mb-3 flex min-h-5 flex-wrap items-center gap-x-3 gap-y-1", flush && "px-5")}>
          {label != null && <span className="min-w-0 flex-auto type-footnote text-muted-foreground">{label}</span>}
          {meta != null && <span className="min-w-0 font-mono text-[12px] tabular-nums text-muted-foreground">{meta}</span>}
          {right}
        </div>
      )}
      {children}
    </div>
  )
}

export type Stat = { label: ReactNode; value: ReactNode; sub?: ReactNode; tone?: Tone; k?: string; title?: string }
/** figures side by side: a grey label over a monospace number, hairlines between */
export function StatStrip({ items, cols, className }: { items: (Stat | null | false | undefined)[]; cols?: number; className?: string }) {
  const xs = items.filter(Boolean) as Stat[]
  const n = cols ?? xs.length
  return (
    <div className={cn("grid", n >= 4 ? "grid-cols-2 sm:grid-cols-4" : n === 3 ? "grid-cols-3" : n === 2 ? "grid-cols-2" : "grid-cols-1", className)} data-stat-strip>
      {xs.map((s, i) => (
        <div key={s.k ?? i} title={s.title} className={cn("min-w-0 px-5 py-3.5", "border-border [&:not(:first-child)]:border-l", n >= 4 && "max-sm:[&:nth-child(2n+1)]:border-l-0 max-sm:[&:nth-child(n+3)]:border-t")} data-stat={s.k}>
          <div className="truncate type-footnote text-muted-foreground">{s.label}</div>
          <div className={cn("mt-1 truncate font-mono text-[17px] leading-6 tabular-nums tracking-[-0.01em]", s.tone ? VALUE[s.tone] : "text-foreground")}>{s.value}</div>
          {s.sub != null && <div className="mt-0.5 truncate type-footnote text-muted-foreground">{s.sub}</div>}
        </div>
      ))}
    </div>
  )
}

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
      <span className={cn("block h-full rounded-full", tone === "bad" ? "bg-destructive" : tone === "wait" ? "bg-attention" : tone === "ok" ? "bg-success" : tone === "run" ? "bg-blue" : "bg-foreground/70")} style={{ width: `${v * 100}%` }} />
    </span>
  )
}

/** a small line of a series (values 0..1, gaps break the line) */
export function Spark({ values, label, className, color = "currentColor" }: { values: (number | null | undefined)[]; label: string; className?: string; color?: string }) {
  if (values.filter((v) => v != null).length < 2) return <span className={cn("h-6 w-20 shrink-0", className)} aria-hidden />
  const n = Math.max(1, values.length - 1)
  const pts = values.map((v, i) => (v == null ? null : ([(i / n) * 100, 22 - Math.min(1, Math.max(0, v)) * 20] as const)))
  const d = pts.reduce((s, p, i) => (p ? s + (i && pts[i - 1] ? "L" : "M") + p[0].toFixed(1) + " " + p[1].toFixed(1) : s), "")
  return (
    <svg viewBox="0 0 100 24" preserveAspectRatio="none" className={cn("h-6 w-20 shrink-0 text-muted-foreground", className)} role="img" aria-label={label}>
      <path d={d} fill="none" stroke={color} strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" vectorEffect="non-scaling-stroke" />
    </svg>
  )
}

/** a list row inside a panel: name and grey sub on the left, anything on the right (a spark, a figure with its meter) */
/** a row of a panel; with `onClick` the row opens something. `controls`: its children are buttons of their own, so the row
 *  is a plain box under a full-size open button instead of one button around them (no button inside a button) */
export function PanelRow({ title, sub, children, onClick, className, controls = false, ...rest }: { title: ReactNode; sub?: ReactNode; children?: ReactNode; onClick?: () => void; className?: string; controls?: boolean; [k: string]: unknown }) {
  const body = <>
    <span className="min-w-0 flex-1">
      <span className="block truncate type-callout text-foreground">{title}</span>
      {sub != null && <span className="mt-0.5 block truncate type-footnote text-muted-foreground">{sub}</span>}
    </span>
    {children}
  </>
  const cls = cn("flex w-full min-w-0 items-center gap-4 px-5 py-2.5 text-left [&+&]:border-t [&+&]:border-border/60", onClick && "cursor-pointer transition-colors hover:bg-muted/40 focus-visible:bg-muted/40 focus-visible:outline-none", className)
  if (onClick && controls) return (
    <div className={cn(cls, "relative has-[[data-row-cover]:focus-visible]:bg-muted/40")} {...rest}>
      <button type="button" onClick={onClick} aria-label={typeof title === "string" ? title : undefined} className="absolute inset-0 cursor-pointer outline-none" data-row-cover />
      <span className="pointer-events-none relative min-w-0 flex-1">
        <span className="block truncate type-callout text-foreground">{title}</span>
        {sub != null && <span className="mt-0.5 block truncate type-footnote text-muted-foreground">{sub}</span>}
      </span>
      <span className="relative flex shrink-0 items-center gap-4">{children}</span>
    </div>)
  return onClick ? <button type="button" onClick={onClick} className={cls} {...rest}>{body}</button> : <div className={cls} {...rest}>{body}</div>
}

/** a figure with its meter under it, right-aligned (the “FP rate” column) */
export function MeterValue({ value, ratio, tone, className }: { value: ReactNode; ratio: number | null | undefined; tone?: Tone; className?: string }) {
  return (
    <span className={cn("flex w-16 shrink-0 flex-col items-end gap-1.5", className)}>
      <span className="font-mono text-[13px] leading-4 tabular-nums text-foreground">{value}</span>
      <Meter value={ratio} tone={tone} className="w-12" />
    </span>
  )
}
