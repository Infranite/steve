/* The pieces the configuration pages share (Bot, 设置, 自动化, 账号, 保险箱, 我的, 视觉规范), so they read as one
 * family: a section is a quiet label over a panel; a panel is one lifted card that holds rows with hairlines only
 * between them; notes, notices and an inline "nothing here" are muted fills, never a box drawn inside a box. */
import { Children, useEffect, type ComponentProps, type ReactNode } from "react"
import { ChevronRightIcon, CircleAlertIcon, InfoIcon, TriangleAlertIcon, CircleCheckIcon } from "lucide-react"
import { cn } from "cn"
import { Spinner } from "@/components/ui/spinner"
import { Collapsible, CollapsibleContent } from "@/components/ui/collapsible"
import { Group, SectionLabel as UISectionLabel, useScrollMore } from "@/ui"

/** a lifted surface: the card of visual system v2 (white in light, one step up with a top highlight in dark) */
export const LIFT = "rounded-xl bg-card shadow-[var(--lift)]"

/** rows in a card. The rows keep their own hover wash, inset by the card's 4px so the corners stay concentric
 *  (12 outside, 8 inside); the hairlines between rows line up with the text. */
export function Panel({ children, className, ...rest }: { children: ReactNode; className?: string } & Omit<ComponentProps<"div">, "children">) {
  if (!Children.toArray(children).filter(Boolean).length) return null
  return (
    <div data-panel className={cn(LIFT, "p-1", className)} {...rest}>
      <Group className="mx-0 [&>[data-slot=item-separator]]:mx-3 [&>[data-slot=item]]:px-3 [&>div>[data-slot=item]]:px-3">{children}</Group>
    </div>
  )
}

/** a card for content that is not a list of rows */
export function Card({ children, className, ...rest }: { children: ReactNode; className?: string } & Omit<ComponentProps<"div">, "children">) {
  return <div className={cn(LIFT, "p-4", className)} {...rest}>{children}</div>
}

/** a section: its label (with an optional control on the right), what it holds, and an optional note under it */
export function Section({ label, right, note, children, className, ...rest }: { label?: ReactNode; right?: ReactNode; note?: ReactNode; children: ReactNode; className?: string } & Omit<ComponentProps<"section">, "children">) {
  return (
    <section className={cn("min-w-0", className)} {...rest}>
      {label != null && <SectionLabel right={right}>{label}</SectionLabel>}
      {children}
      {note && <Note>{note}</Note>}
    </section>
  )
}

/** the label over a section: quiet, a little in from the panel's edge so it lines up with the rows' text */
export function SectionLabel({ className, ...rest }: ComponentProps<typeof UISectionLabel>) {
  return <UISectionLabel className={cn("px-1 text-tertiary", className)} {...rest} />
}

/** a short explanation under a panel or a field */
export function Note({ children, className, ...rest }: { children: ReactNode; className?: string } & Omit<ComponentProps<"div">, "children">) {
  return <div className={cn("mt-2 px-1 type-footnote text-muted-foreground", className)} {...rest}>{children}</div>
}

/** the lead paragraph of a pane, above its first section */
export function Lead({ children, className, ...rest }: { children: ReactNode; className?: string } & Omit<ComponentProps<"p">, "children">) {
  return <p className={cn("type-callout text-muted-foreground", className)} {...rest}>{children}</p>
}

/** "nothing here yet", inline in a section (a page-sized empty state is `Empty`) */
export function Blank({ children, className, ...rest }: { children: ReactNode; className?: string } & Omit<ComponentProps<"div">, "children">) {
  return <div className={cn("rounded-xl bg-muted/60 px-4 py-3.5 type-callout text-muted-foreground", className)} {...rest}>{children}</div>
}

const TONE = {
  info: ["bg-blue/10", "text-blue", InfoIcon],
  attention: ["bg-attention/12", "text-attention", TriangleAlertIcon],
  destructive: ["bg-destructive/10", "text-destructive", CircleAlertIcon],
  success: ["bg-success/10", "text-success", CircleCheckIcon],
  neutral: ["bg-muted/70", "text-muted-foreground", InfoIcon],
} as const
/** a tinted notice: the status color at a tenth for the fill and in full for the icon; the words stay readable */
export function Notice({ tone = "neutral", children, action, className, icon = true, ...rest }: { tone?: keyof typeof TONE; children: ReactNode; action?: ReactNode; className?: string; icon?: boolean } & Omit<ComponentProps<"div">, "children">) {
  const [bg, fg, I] = TONE[tone]
  return (
    <div className={cn("flex items-start gap-2.5 rounded-xl px-4 py-3 type-callout text-foreground/90", bg, className)} {...rest}>
      {icon && <I className={cn("mt-0.5 size-4 shrink-0", fg)} strokeWidth={2} aria-hidden />}
      <div className="min-w-0 flex-1">{children}</div>
      {action && <div className="-my-1 shrink-0">{action}</div>}
    </div>
  )
}

/** a small tinted capsule for a state or a mark beside a title */
export function Pill({ tone = "neutral", children, className, ...rest }: { tone?: keyof typeof TONE; children: ReactNode; className?: string } & Omit<ComponentProps<"span">, "children">) {
  const [bg, fg] = TONE[tone]
  return <span className={cn("inline-flex h-5 shrink-0 items-center gap-1 whitespace-nowrap rounded-md px-1.5 type-label leading-none", bg, tone === "neutral" ? "text-muted-foreground" : fg, className)} {...rest}>{children}</span>
}

/** the icon tile at the start of a row */
export function Tile({ I, className }: { I: any; className?: string }) {
  return <span className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground", className)} aria-hidden><I className="size-4" strokeWidth={1.75} /></span>
}

/** the chevron at the end of a row that opens something */
export const Chevron = () => <ChevronRightIcon className="size-4 shrink-0 text-tertiary" aria-hidden />

/** loading, centred where the content will be */
export const Loading = ({ className }: { className?: string }) => <div className={cn("flex justify-center py-16", className)}><Spinner className="size-5 text-muted-foreground" /></div>

/** the motion of things that open in place: 180ms on the system's ease-out (a chevron turning with its panel) */
export const EASE = "duration-180 ease-(--ease-out)"

/** what opens under a row or a toggle: its height eases open and shut (the content is mounted only while open or
 *  closing, so what was hidden is not in the page). Horizontal padding keeps a lifted panel's ring unclipped. */
export function Reveal({ open, children, className, ...rest }: { open: boolean; children: ReactNode; className?: string } & Omit<ComponentProps<typeof CollapsibleContent>, "children" | "className">) {
  return (
    <Collapsible open={open}>
      <CollapsibleContent className={cn("-mx-1 h-(--collapsible-panel-height) overflow-hidden px-1 transition-[height,opacity] duration-220 ease-(--ease-out) data-ending-style:h-0 data-ending-style:opacity-0 data-starting-style:h-0 data-starting-style:opacity-0", className)} {...rest}>
        {children}
      </CollapsibleContent>
    </Collapsible>
  )
}

/** a chevron that turns with what it opens: right → down, or down → up with `flip` */
export const Turn = ({ open, flip, className }: { open: boolean; flip?: boolean; className?: string }) => (
  <ChevronRightIcon className={cn("size-4 shrink-0 text-tertiary transition-transform", EASE, flip ? (open ? "-rotate-90" : "rotate-90") : open && "rotate-90", className)} aria-hidden />
)

/** facts in one line, "a · b · c": the dot belongs to the part after it, and the dot that would start a wrapped
 *  line sits outside the clip, so a line never ends or starts with a dangling "·" */
export function MetaLine({ children, className, one }: { children: ReactNode; className?: string; one?: boolean }) {
  const xs = Children.toArray(children).filter(Boolean)
  return (
    <div className={cn("min-w-0 overflow-hidden type-footnote text-muted-foreground", className)}>
      <div className={cn("-ml-4 flex min-w-0", one ? "flex-nowrap" : "flex-wrap")}>
        {xs.map((x, n) => <span key={n} className={cn("min-w-0 before:inline-block before:w-4 before:text-center before:text-tertiary before:content-['·']", one && n === xs.length - 1 ? "truncate" : "shrink-0 whitespace-nowrap", !one && "max-w-full truncate")}>{x}</span>)}
      </div>
    </div>
  )
}

/** a field and its button as the last row of a panel: one way to add or enter something, everywhere */
export function FieldRow({ children, className, ...rest }: { children: ReactNode; className?: string } & Omit<ComponentProps<"form">, "children">) {
  return <form className={cn("flex min-w-0 flex-wrap items-center gap-2 px-3 py-2.5 sm:flex-nowrap [&_[data-slot=input]]:h-8 [&_[data-slot=input]]:min-w-0 [&_[data-slot=input]]:flex-1", className)} {...rest}>{children}</form>
}

/** a strip that may be wider than the screen (tabs, a long segmented control): it scrolls sideways with a fade on
 *  the side that has more, and keeps the selected tab in view */
export function Strip({ children, className, watch }: { children: ReactNode; className?: string; watch?: unknown }) {
  const [ref, more] = useScrollMore<HTMLDivElement>("x")
  useEffect(() => {
    const el = ref.current, on = el?.querySelector<HTMLElement>('[aria-selected="true"], [data-active], [aria-pressed="true"]')
    if (!el || !on) return
    const a = el.getBoundingClientRect(), b = on.getBoundingClientRect()
    if (b.left < a.left || b.right > a.right) el.scrollLeft += b.left - a.left - (a.width - b.width) / 2
  }, [watch])
  const mask = more === "end" ? "linear-gradient(to right, black 85%, transparent)" : more === "start" ? "linear-gradient(to left, black 85%, transparent)" : more ? "linear-gradient(to right, transparent, black 12%, black 88%, transparent)" : undefined
  return <div ref={ref} className={cn("-mx-1 max-w-[calc(100%+0.5rem)] overflow-x-auto px-1 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden", className)} style={mask ? { maskImage: mask, WebkitMaskImage: mask } : undefined} data-more={more}>{children}</div>
}
