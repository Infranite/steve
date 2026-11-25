import { getItem } from "@/store"
/* Shared building blocks for the Steve prototype — see DESIGN.md.
 * Quiet chrome: grey scale everywhere, blue = selected/running, orange = needs you. */
import { Children, Fragment, useEffect, useLayoutEffect, useId, useRef, useState, useSyncExternalStore, type ComponentProps, type InputHTMLAttributes, type ReactNode, type Ref } from "react"
import { flushSync } from "react-dom"
import { Robot } from "@/robot"
import { MineButton } from "@/mine"
import { LANGS, lang, setLang, useT, type Lang } from "@/i18n"
import { cn } from "cn"
import { itemDisplayStatus } from "@/store"
import { ScrollArea } from "@/components/affect/scroll-area-clean"
import { ChevronLeftIcon, ListTodoIcon, MoonIcon, SearchIcon, SunIcon, TerminalIcon, MonitorIcon, XIcon, CircleCheckIcon, ChevronRightIcon, CircleAlertIcon, InfoIcon, TriangleAlertIcon, CircleAlertIcon as RelFailIcon, CircleCheckIcon as RelOkIcon, MegaphoneIcon as RelRunIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { useFieldLabels } from "@/components/ui/input"
import { TextField as AffectTextField } from "@/components/affect/text-field-clean"
import { Item as UIItem, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemSeparator, ItemTitle } from "@/components/ui/item"
import { Empty as UIEmpty } from "@/components/ui/empty"
import { Card as AffectCard } from "@/components/affect/card-clean"
import { Breadcrumb } from "@/components/affect/breadcrumb-clean"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { SegmentedControl } from "@/components/affect/segmented-control-clean"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { Spinner } from "@/components/ui/spinner"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { st, emit, ST, SS, tagCatLabel, tagLabel, tagDesc, botName, tag, isKind, sortTags, type Rel } from "@/store"
import "@/themes" /* the appearance chosen on this device, on <html> before the first paint */
import { RunningNowPill } from "@/running-pill"
import { reducedMotion } from "@/lib/motion"

/* ───────── routing ───────── */
/* a route change cross-fades the content area (view-transition-name: page) while the sidebar stays; without view
 * transitions, or with reduced motion, it just changes */
const hashSub = (cb: () => void) => {
  const on = () => {
    const d = document as Document & { startViewTransition?: (f: () => void) => { ready: Promise<void>; finished: Promise<void> } }
    /* a transition the browser skips (a second route change before the first one drew) rejects its promises: nothing failed */
    if (d.startViewTransition && !reducedMotion()) { const v = d.startViewTransition(() => flushSync(cb)); v.ready.catch(() => {}); v.finished.catch(() => {}) }
    else cb()
  }
  window.addEventListener("hashchange", on); return () => window.removeEventListener("hashchange", on)
}
export function useRoute() { const h = useSyncExternalStore(hashSub, () => location.hash); return h.replace(/^#\/?/, "").split("/") }
export const go = (h: string) => { location.hash = h }

/* ───────── theme (light default, follows system) ───────── */
export type ThemePref = "system" | "light" | "dark"
let themePref: ThemePref = (localStorage.getItem("steve-theme") as ThemePref) || "system"
const mq = window.matchMedia("(prefers-color-scheme: dark)")
export const resolvedTheme = () => (themePref === "system" ? (mq.matches ? "dark" : "light") : themePref)
function applyTheme() { document.documentElement.classList.toggle("dark", resolvedTheme() === "dark"); document.documentElement.style.colorScheme = resolvedTheme() }
applyTheme(); mq.addEventListener("change", () => { applyTheme(); emit() })
export const getTheme = () => themePref
export function setTheme(t: ThemePref) { themePref = t; localStorage.setItem("steve-theme", t); applyTheme(); emit() }
export function ThemeSwitch({ className, labels = false }: { className?: string; labels?: boolean }) {
  const t = useT()
  const o = (v: ThemePref, I: any, l: string) => [v, <span key={v} className="inline-flex items-center gap-1.5"><I className="size-3.5" />{labels && l}</span>, l] as const
  return <Segmented className={className} value={themePref} onChange={(v) => setTheme(v as ThemePref)} iconOnly={!labels} size="sm"
    options={[o("light", SunIcon, t("core:theme.light")), o("dark", MoonIcon, t("core:theme.dark")), o("system", MonitorIcon, t("core:theme.system"))]} />
}
/** the UI language, remembered on this device; each name is written in its own language */
export function LanguageSwitch({ className }: { className?: string }) {
  const t = useT()
  return <Segmented className={className} label={t("common:settings.language")} value={lang()} onChange={(v: Lang) => setLang(v)} size="sm"
    options={LANGS.map((l) => [l, <span key={l} lang={l}>{t(`common:lang.${l}`)}</span>] as const)} />
}

/* ───────── atoms ───────── */
const DOT: Record<string, string> = { fail: "bg-destructive", block: "border-[1.5px] border-dashed border-tertiary", run: "bg-blue dot-running", prop: "bg-blue", wait: "bg-attention", pause: "bg-tertiary", done: "bg-success", drop: "bg-tertiary", idle: "bg-tertiary", ended: "ring-[1.5px] ring-inset ring-tertiary" }
/** what became of an approved release, in its own color: a failure is never shown as a plain "approved".
 *  `box` is the prominent form for the item page */
export function RelNote({ rel, box, className, onClose }: { rel: Rel; box?: boolean; className?: string; onClose?: () => void }) {
  const I = rel.tone === "fail" ? RelFailIcon : rel.tone === "ok" ? RelOkIcon : RelRunIcon
  const color = rel.tone === "fail" ? "text-destructive" : rel.tone === "ok" ? "text-success" : "text-blue"
  return (
    <div role={rel.tone === "fail" ? "alert" : "status"} data-rel={rel.tone}
      className={cn("flex min-w-0 items-start gap-1.5", box ? "rounded-xl px-3.5 py-2.5 type-callout" : "type-footnote", box && (rel.tone === "fail" ? "bg-destructive/10" : rel.tone === "ok" ? "bg-success/10" : "bg-blue/10"), className)}>
      <I className={cn("mt-0.5 size-3.5 shrink-0", box && "size-4", color)} strokeWidth={2} />
      <span className={cn("min-w-0", box ? "flex-1 text-foreground" : color, !box && "truncate")} title={rel.text}>{rel.text}</span>
      {onClose && <AlertClose onClose={onClose} className={box ? "-my-0.5" : "-my-1"} />}
    </div>
  )
}
/** closes a failure that is no longer worth showing; it stays in the timeline and the notifications */
export function AlertClose({ onClose, className }: { onClose: () => void; className?: string }) {
  const t = useT()
  return (
    <Tooltip>
      <TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("core:alert.close")} className={cn("shrink-0 text-muted-foreground", className)} onClick={(e) => { e.stopPropagation(); onClose() }} onKeyDown={(e) => e.stopPropagation()} data-alert-close />}><XIcon /></TooltipTrigger>
      <TooltipContent>{t("core:alert.closeHint")}</TooltipContent>
    </Tooltip>
  )
}
export function StatusDot({ s, className }: { s: string; className?: string }) {
  return <span aria-hidden className={cn("inline-block size-2 shrink-0 rounded-full", DOT[s] ?? "bg-tertiary", className)} />
}
/** `pill`: a tinted capsule, for the one status a page is about */
export function Status({ s, sess = false, pill = false, className }: { s: string; sess?: boolean; pill?: boolean; className?: string }) {
  const label = (sess ? SS : ST)[s]?.[0] ?? ""
  return <span className={cn("inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap type-footnote text-muted-foreground", pill && cn("h-6 rounded-full px-2.5 font-medium", s === "run" || s === "wait" || s === "fail" || s === "done" ? TONE[toneOf(s)].slice(0, 2) : "bg-muted"), className)} data-status={s}><StatusDot s={s} />{label}</span>
}
export function BotAvatar({ k, size = 24, className }: { k: string | null; size?: number; className?: string; style?: string; seed?: string }) {
  if (k && st.bots[k]) return <Robot k={k} size={size} className={className} />
  return (
    <span className={cn("inline-flex shrink-0 select-none items-center justify-center rounded-full bg-muted text-muted-foreground", className)} style={{ width: size, height: size }} aria-hidden>
      <TerminalIcon style={{ width: size * 0.5, height: size * 0.5 }} strokeWidth={1.75} />
    </span>
  )
}
export function BotName({ k, className }: { k: string | null; className?: string }) {
  if (!k) return null
  return <span className={cn("inline-flex items-center gap-1.5 whitespace-nowrap", className)}><BotAvatar k={k} size={16} />{botName(k)}</span>
}
/** an item's category: a quiet muted pill (no hue: colour on a row means status), always first and never removable on its own */
export function KindTag({ id, className }: { id: string; className?: string }) {
  const tr = useT(), t = tag(id)
  if (!t) return null
  return <span title={tr("core:tag.kindTitle", { desc: tagDesc(t) })} className={cn("inline-flex h-5 shrink-0 items-center gap-1 rounded-md bg-muted px-1.5 type-footnote leading-none text-foreground/80", className)} data-kind={t.name}>
    {t.name}</span>
}
/** tags: type first, then delivery and domain; delivery nature stays visible without a hover */
export function Tags({ ids, className, onRemove, max }: { ids?: string[]; className?: string; onRemove?: (id: string) => void; max?: number }) {
  const tr = useT()
  const xs = sortTags(ids).map(tag).filter(Boolean) as NonNullable<ReturnType<typeof tag>>[]
  if (!xs.length) return null
  const kinds = xs.filter(isKind), rest = xs.filter((t) => !isKind(t))
  const shown = max ? rest.slice(0, max) : rest
  return (
    <span className={cn("inline-flex flex-wrap items-center gap-1", className)} data-tags>
      {kinds.map((t) => <KindTag key={t.id} id={t.id} />)}
      {shown.map((t) => (
        <span key={t.id} title={tr("core:tag.title", { cat: tagCatLabel(t.cat), desc: tagDesc(t) })} className={cn("inline-flex h-5 items-center gap-0.5 rounded-md bg-muted px-1.5 type-footnote leading-none", t.cat === "交付" ? "text-foreground/80" : "text-muted-foreground")} data-tag={t.name} data-delivery={t.cat === "交付" || undefined}>
          {/* in a list the delivery kind reads without its category (the title says it); editing it shows it whole */}
          {t.cat === "交付" && onRemove && <span className="mr-0.5 text-muted-foreground">{tagCatLabel(t.cat)} ·</span>}{tagLabel(t)}{onRemove && <button type="button" aria-label={tr("core:tag.remove", { name: tagLabel(t) })} onClick={(e) => { e.stopPropagation(); onRemove(t.id) }} className="-mr-1 ml-0.5 flex size-4 items-center justify-center rounded-full text-muted-foreground hover:bg-foreground/10 hover:text-foreground"><XIcon className="size-3" /></button>}
        </span>))}
      {max && rest.length > max && <span className="type-footnote text-muted-foreground">+{rest.length - max}</span>}
    </span>
  )
}
export const ItemNo = ({ id, className }: { id: string; className?: string }) => <span className={cn("shrink-0 type-footnote font-mono tabular-nums text-muted-foreground", className)} data-no>{id}</span>
/** a single line of facts, "a · b · c" (`one`): a "·" between parts, as its own element after each part but the last.
 *  The part keeps a dot-wide right margin and the dot, pulled back over it, takes no width of its own, so a part that
 *  truncates, has a border or a gap keeps it whole. Dots around a part that renders nothing collapse. */
const META_SEP = "overflow-x-clip [&>:not([data-meta-sep]):not(:last-child)]:mr-4 [&>[data-meta-sep]:first-child]:hidden [&>[data-meta-sep]:last-child]:hidden [&>[data-meta-sep]+[data-meta-sep]]:hidden"
const metaParts = (children: ReactNode) => Children.toArray(children).flatMap((c, n) => n ? [<span key={"sep" + n} data-meta-sep aria-hidden className="-ml-4 w-4 shrink-0 text-center text-tertiary">·</span>, c] : [c])
/** facts in a line, "a · b · c". By default the line wraps: each dot belongs to the part after it, and the dot that
 *  would start a wrapped line sits in a clipped gutter, so a line never ends or starts with a dangling "·". `one` keeps
 *  them on one line (the parts are the flex row's own children, so they can truncate and yield). `inline` renders spans,
 *  for a Row's subtitle, which is a <p>. */
export function Meta({ children, className, one, inline }: { children: ReactNode; className?: string; one?: boolean; inline?: boolean }) {
  const Box = inline ? "span" : "div"
  if (one) return <Box className={cn("flex min-w-0 flex-nowrap items-center type-footnote text-muted-foreground", META_SEP, className)} data-meta>{metaParts(children)}</Box>
  const xs = Children.toArray(children).filter(Boolean)
  return (
    <Box className={cn("block min-w-0 overflow-hidden type-footnote text-muted-foreground", className)} data-meta>
      <Box className="-ml-4 flex min-w-0 flex-wrap items-center gap-y-1">
        {xs.map((x, n) => <span key={n} data-meta-part className="inline-flex min-w-0 max-w-full items-center whitespace-nowrap empty:hidden before:inline-block before:w-4 before:shrink-0 before:text-center before:text-tertiary before:content-['·']">{x}</span>)}
      </Box>
    </Box>
  )
}
export function SectionLabel({ children, right, className }: { children: ReactNode; right?: ReactNode; className?: string }) {
  return <div className={cn("mb-2 flex min-h-6 items-center justify-between gap-2 px-1 type-label text-tertiary", className)}><span>{children}</span>{right}</div>
}
/** grouped list: shadcn ItemGroup in one hairline container, ItemSeparator between rows */
export function Group({ children, className }: { children: ReactNode; className?: string }) {
  const xs = Children.toArray(children).filter(Boolean)
  return (
    <ItemGroup className={cn("-mx-2 w-auto gap-0!", className)}>
      {xs.map((c, n) => <Fragment key={n}>{n > 0 && <ItemSeparator className="mx-2 my-0 w-auto!" />}{c}</Fragment>)}
    </ItemGroup>
  )
}
/** `controls`: the children hold their own buttons. A button cannot contain a button, so the row's click target is
 *  then a cover button under the content, and the children sit above it. */
export { permissionCommand, permissionQuestion } from "@/permission-question"
export function Row({ title, sub, children, onClick, icon, className, controls = false, ...rest }: { title: ReactNode; sub?: ReactNode; children?: ReactNode; onClick?: () => void; icon?: ReactNode; className?: string; controls?: boolean; [k: string]: any }) {
  const id = useId(), cover = !!onClick && controls
  return (
    <UIItem size="sm" role="listitem" className={cn("min-h-12 rounded-lg border-0 px-2 py-2.5", onClick && "cursor-pointer text-left transition-colors duration-120 ease-(--ease-out) hover:bg-accent active:bg-accent/70", cover && "relative has-[[data-row-cover]:focus-visible]:ring-[3px] has-[[data-row-cover]:focus-visible]:ring-ring/50", className)}
      render={onClick && !cover ? <button type="button" onClick={onClick} /> : undefined} {...rest}>
      {cover && <button type="button" onClick={onClick} aria-labelledby={id} className="absolute inset-0 cursor-pointer rounded-[inherit] outline-none" data-row-cover />}
      {icon && <ItemMedia>{icon}</ItemMedia>}
      <ItemContent className="min-w-0 gap-0.5">
        <ItemTitle id={id} className="w-full type-body font-normal">{title}</ItemTitle>
        {sub && <ItemDescription className="type-footnote text-muted-foreground">{sub}</ItemDescription>}
      </ItemContent>
      {children && <ItemActions className={cn("shrink-0", cover && "relative")}>{children}</ItemActions>}
    </UIItem>
  )
}
/** view switcher: affectUI's Segmented Control, a raised pill sliding along a muted track. It is a tablist (role tab,
 *  aria-selected); `pressed`: the same control as a radio group, for a filter that does not switch views; `full`
 *  stretches it to its container with equal segments. An option is a value, or [value, label, tooltip] – with
 *  `iconOnly` the label is an icon and the tooltip names it. data-seg on each option, data-slot tabs/tabs-list/
 *  tabs-trigger as before. */
export function Segmented<T extends string>({ value, onChange, options, className, iconOnly = false, size = "default", label, pressed = false, full = false }: { value: T; onChange: (v: T) => void; options: readonly (T | readonly [T, ReactNode, string?])[]; className?: string; iconOnly?: boolean; size?: "default" | "sm"; label?: string; pressed?: boolean; full?: boolean }) {
  const opts = options.map((o) => {
    const [v, l, tip] = typeof o === "string" ? [o, o, undefined] : o
    return { value: v as T, ...(iconOnly ? { icon: l } : { label: l }), ariaLabel: tip, title: tip, data: { "data-seg": v, "data-slot": "tabs-trigger" } }
  })
  return (
    <SegmentedControl options={opts} value={value} onChange={onChange} size={size === "sm" ? "md" : "lg"} variant="tray" fullWidth={full} kind={pressed ? "radio" : "tabs"}
      aria-label={label} className={cn("steve-seg", className)} data={{ "data-slot": "tabs" }} trackData={{ "data-slot": "tabs-list" }} />
  )
}
/** filters (shadcn ToggleGroup) — single choice, never empty */
export function Chips<T extends string>({ value, onChange, options, className }: { value: T; onChange: (v: T) => void; options: readonly T[]; className?: string }) {
  return (
    <ToggleGroup value={[value]} onValueChange={(v: any[]) => v.length && onChange(v[v.length - 1] as T)} size="sm" spacing={1} className={className}>
      {options.map((o) => <ToggleGroupItem key={o} value={o} data-seg={o} className="h-7 rounded-lg px-2.5 type-callout text-muted-foreground hover:bg-accent hover:text-foreground data-pressed:bg-accent data-pressed:font-medium data-pressed:text-foreground">{o}</ToggleGroupItem>)}
    </ToggleGroup>
  )
}
/** `action`: what to do about it, inside the empty state rather than a page-height below it. `compact`: the same, sized
 *  for a panel or a section on a muted fill instead of a whole page */
export function Empty({ title, sub, icon, action, compact = false, className, ...rest }: { title: ReactNode; sub?: ReactNode; icon?: ReactNode; action?: ReactNode; compact?: boolean; className?: string } & Record<`data-${string}`, unknown>) {
  return (
    <UIEmpty bare={!compact} icon={icon || null} action={action || null} data-empty={compact ? "compact" : "page"} {...rest}
      className={cn("anim-in w-full min-w-0 justify-center text-balance", compact ? "rounded-xl px-4 py-8" : "flex-1 px-4 py-16 sm:px-12", className)}
      title={<span className={compact ? "type-body font-medium" : "type-headline"}>{title}</span>}
      text={sub ? <span className={compact ? "type-footnote" : "type-callout"}>{sub}</span> : null} />
  )
}

/* ───────── surfaces (visual system v2): a section is a quiet label over a panel; a panel is one lifted card that
 * holds rows with hairlines only between them; notes, notices and an inline "nothing here" are muted fills, never a
 * box drawn inside a box ───────── */
/** a lifted surface: the card of visual system v2 (white in light, one step up with a top highlight in dark). Panel and
 *  Card below are affectUI's Card (steve-card: block flow and the page's own type, see affectui-placement.css); LIFT
 *  stays for the few surfaces that are not cards (a popover-like strip, a table frame) */
export const LIFT = "rounded-xl bg-card shadow-[var(--lift)]"

/** rows in a card. The rows keep their own hover wash, inset by the card's 4px so the corners stay concentric
 *  (12 outside, 8 inside); the hairlines between rows line up with the text. */
export function Panel({ children, className, ...rest }: { children: ReactNode; className?: string } & Omit<ComponentProps<"div">, "children">) {
  if (!Children.toArray(children).filter(Boolean).length) return null
  return (
    <AffectCard data-panel className={cn("steve-card p-1", className)} {...rest}>
      <Group className="mx-0 [&>[data-slot=item-separator]]:mx-3 [&>[data-slot=item]]:px-3 [&>div>[data-slot=item]]:px-3">{children}</Group>
    </AffectCard>
  )
}

/** a fact: its label, its value, and (when it can be changed) where it is changed; without `onClick` it is only read */
export function Fact({ label, children, onClick, testId }: { label: string; children: ReactNode; onClick?: () => void; testId: string }) {
  const row = "flex w-full min-w-0 items-start gap-3 rounded-lg px-3 py-2.5 text-left max-sm:flex-col max-sm:gap-0.5"
  const body = (<>
    <span className="w-28 shrink-0 pt-px type-callout text-muted-foreground">{label}</span>
    <span className="flex min-w-0 flex-1 items-start gap-2 self-stretch">
      <span className="min-w-0 flex-1 type-body text-foreground">{children}</span>
      {onClick && <ChevronRightIcon className="mt-1 size-4 shrink-0 text-tertiary opacity-60 transition-opacity group-hover/fact:opacity-100 max-sm:hidden" aria-hidden />}
    </span>
  </>)
  if (!onClick) return <div className={row} data-fact={testId}>{body}</div>
  return (
    <button type="button" onClick={onClick} data-fact={testId}
      className={cn("group/fact transition-colors duration-120 ease-(--ease-out) hover:bg-accent active:bg-accent/70", row)}>{body}</button>
  )
}
/** one figure of a recent period */
export function Figure({ label, value, sub, testId }: { label: string; value: string; sub?: string; testId: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-1 px-4 py-3" data-figure={testId}>
      <span className="type-footnote text-muted-foreground">{label}</span>
      <span className={cn("type-title tabular-nums", value === "—" && "text-tertiary")}>{value}</span>
      {sub && <span className="truncate type-footnote text-tertiary">{sub}</span>}
    </div>
  )
}
/** a card for content that is not a list of rows */
export function Card({ children, className, ...rest }: { children: ReactNode; className?: string } & Omit<ComponentProps<"div">, "children">) {
  return <AffectCard className={cn("steve-card p-4", className)} {...rest}>{children}</AffectCard>
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

/** a short explanation under a panel or a field */
export function Note({ children, className, ...rest }: { children: ReactNode; className?: string } & Omit<ComponentProps<"div">, "children">) {
  return <div className={cn("mt-2 px-1 type-footnote text-muted-foreground", className)} {...rest}>{children}</div>
}

/** the lead paragraph of a pane, above its first section */
export function Lead({ children, className, ...rest }: { children: ReactNode; className?: string } & Omit<ComponentProps<"p">, "children">) {
  return <p className={cn("type-callout text-muted-foreground", className)} {...rest}>{children}</p>
}

/** "nothing here yet" in one line, inline in a section (with an icon and an action it is `Empty compact`; a page-sized
 *  empty state is `Empty`) */
export function Blank({ children, className, ...rest }: { children: ReactNode; className?: string } & Omit<ComponentProps<"div">, "children">) {
  return <div className={cn("rounded-xl bg-muted/60 px-4 py-3.5 type-callout text-muted-foreground", className)} {...rest}>{children}</div>
}

/** the tints of a state: the status color at a tenth for the fill and in full for the icon and the words of a pill.
 *  Item and session states (run, wait, fail, done, quiet) map onto the same four hues. */
const TONE = {
  info: ["bg-blue/10", "text-blue", InfoIcon],
  attention: ["bg-attention/12", "text-attention", TriangleAlertIcon],
  destructive: ["bg-destructive/10", "text-destructive", CircleAlertIcon],
  success: ["bg-success/10", "text-success", CircleCheckIcon],
  neutral: ["bg-muted/70", "text-muted-foreground", InfoIcon],
} as const
export type Tone = keyof typeof TONE | "run" | "wait" | "fail" | "done" | "quiet"
const toneOf = (t: Tone): keyof typeof TONE => (t === "run" ? "info" : t === "wait" ? "attention" : t === "fail" ? "destructive" : t === "done" ? "success" : t === "quiet" ? "neutral" : t)
/** a tinted notice: the status color at a tenth for the fill and in full for the icon; the words stay readable */
export function Notice({ tone = "neutral", children, action, className, icon = true, ...rest }: { tone?: Tone; children: ReactNode; action?: ReactNode; className?: string; icon?: boolean } & Omit<ComponentProps<"div">, "children">) {
  const [bg, fg, I] = TONE[toneOf(tone)]
  return (
    <div className={cn("flex items-start gap-2.5 rounded-xl px-4 py-3 type-callout text-foreground/90", bg, className)} {...rest}>
      {icon && <I className={cn("mt-0.5 size-4 shrink-0", fg)} strokeWidth={2} aria-hidden />}
      <div className="min-w-0 flex-1">{children}</div>
      {action && <div className="-my-1 shrink-0">{action}</div>}
    </div>
  )
}

/** the status pill: a small tinted capsule for a state or a mark beside a title */
export function Pill({ tone = "neutral", children, className, ...rest }: { tone?: Tone; children: ReactNode; className?: string } & Omit<ComponentProps<"span">, "children">) {
  const k = toneOf(tone), [bg, fg] = TONE[k]
  return <span className={cn("inline-flex h-5 shrink-0 items-center gap-1 whitespace-nowrap rounded-md px-1.5 type-label leading-none", bg, k === "neutral" ? "text-muted-foreground" : fg, className)} {...rest}>{children}</span>
}

/** the icon tile at the start of a row: one size everywhere (32px, the button radius) */
export function Tile({ I, className, children }: { I?: any; className?: string; children?: ReactNode }) {
  return <span className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground", className)} aria-hidden>{I ? <I className="size-4" strokeWidth={1.75} /> : children}</span>
}

/** the chevron at the end of a row that opens something */
export const Chevron = () => <ChevronRightIcon className="size-4 shrink-0 text-tertiary" aria-hidden />

/** loading, centred where the content will be */
export const Loading = ({ className }: { className?: string }) => <div className={cn("flex justify-center py-16", className)}><Spinner className="size-5 text-muted-foreground" /></div>

/** the motion of things that open in place: 180ms on the system's ease-out (a chevron turning with its panel) */
export const EASE = "duration-180 ease-(--ease-out)"

/** what opens under a row or a toggle: its height eases open and shut (the content is mounted only while open or
 *  closing, so what was hidden is not in the page) and it fades with it. Horizontal padding keeps a lifted panel's
 *  ring unclipped. */
export function Reveal({ open, children, className, ...rest }: { open: boolean; children: ReactNode; className?: string } & Omit<ComponentProps<typeof CollapsibleContent>, "children" | "className">) {
  return (
    <Collapsible open={open}>
      <CollapsibleContent className={cn("-mx-1 px-1 transition-[height,opacity] data-ending-style:opacity-0 data-starting-style:opacity-0", className)} {...rest}>
        {children}
      </CollapsibleContent>
    </Collapsible>
  )
}

/** a chevron that turns with what it opens: right → down, or down → up with `flip` */
export const Turn = ({ open, flip, className }: { open: boolean; flip?: boolean; className?: string }) => (
  <ChevronRightIcon className={cn("size-4 shrink-0 text-tertiary transition-transform", EASE, flip ? (open ? "-rotate-90" : "rotate-90") : open && "rotate-90", className)} aria-hidden />
)

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

/** the search field: Steve's Input (affectUI's Text Field) with the magnifier inside, the one search look on every
 *  page. `sm` for a toolbar beside other small controls, `lg` for a page whose main thing is the search. */
export function SearchField({ className, trailing, inputRef, size = "default", clearable, onClear, ...input }: Omit<InputHTMLAttributes<HTMLInputElement>, "size" | "prefix"> & { trailing?: ReactNode; inputRef?: Ref<HTMLInputElement>; size?: "default" | "sm" | "lg"; clearable?: boolean; onClear?: () => void }) {
  const labels = useFieldLabels()
  return (
    <AffectTextField {...input} ref={inputRef} labels={labels} clearable={clearable} onClear={onClear} size={size === "sm" ? "sm" : size === "lg" ? "lg" : "md"}
      className={cn("min-w-0", className)} fieldProps={{ "data-search-field": true, role: "search" }}
      prefix={<SearchIcon className={size === "sm" ? "size-3.5" : "size-4"} aria-hidden />}
      suffix={trailing != null && trailing !== false ? <span className="inline-flex items-center ps-1.5">{trailing}</span> : undefined} />
  )
}

/* ───────── connect pages: the "nothing connected yet" prompt and the in-place disclosure ───────── */
/** "nothing connected yet": a lifted card, a quiet icon tile, the title, one line of why, then the one thing to do */
export function ConnectPrompt({ icon, title, sub, action, children, className, ...rest }: { icon?: ReactNode; title: ReactNode; sub?: ReactNode; action?: ReactNode; children?: ReactNode; className?: string; [k: `data-${string}`]: unknown }) {
  return (
    <section className={cn("flex items-start gap-3 rounded-xl bg-card p-5 shadow-[var(--lift)]", className)} {...rest}>
      {icon && <Tile className="[&_svg]:size-4">{icon}</Tile>}
      <div className="flex min-w-0 flex-1 flex-col gap-3">
        <div className="flex flex-col gap-1">
          <div className="type-headline text-foreground">{title}</div>
          {sub && <div className="type-callout text-muted-foreground">{sub}</div>}
        </div>
        {children}
        {action && <div className="flex flex-wrap items-center gap-2">{action}</div>}
      </div>
    </section>
  )
}

/** a chevron that turns when its trigger (aria-expanded) or details (open) opens */
export const DiscChevron = ({ className }: { className?: string }) => (
  <ChevronRightIcon aria-hidden className={cn("size-3.5 shrink-0 transition-transform duration-180 ease-(--ease-out) group-aria-expanded/disc:rotate-90 group-open/disc:rotate-90", className)} />
)
/** a disclosure trigger reads as one, not as a link: muted text with a turning chevron */
export const DISC_TRIGGER = "group/disc inline-flex items-center gap-1 rounded-md type-callout text-muted-foreground outline-none hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50"
/** a native <summary> styled like the disclosure trigger, browser marker hidden; put `group/disc` on its <details> */
export const SUMMARY = "inline-flex cursor-pointer list-none items-center gap-1 rounded-md text-muted-foreground outline-none select-none hover:text-foreground focus-visible:ring-[3px] focus-visible:ring-ring/50 [&::-webkit-details-marker]:hidden"

/** content that opens in place, its height easing both ways */
export function Disclosure({ label, children, open, onOpenChange, defaultOpen, className, triggerClassName, ...rest }: { label: ReactNode; children: ReactNode; open?: boolean; onOpenChange?: (o: boolean) => void; defaultOpen?: boolean; className?: string; triggerClassName?: string; [k: `data-${string}`]: unknown }) {
  return (
    <Collapsible open={open} onOpenChange={onOpenChange ? (o) => onOpenChange(o) : undefined} defaultOpen={defaultOpen} className={className} {...rest}>
      <CollapsibleTrigger className={cn(DISC_TRIGGER, triggerClassName)}><DiscChevron />{label}</CollapsibleTrigger>
      <CollapsibleContent>{children}</CollapsibleContent>
    </Collapsible>
  )
}

/** the width of a panel beside a page */
export const PANEL_W = "w-[320px]"
/** Allocate the final docked width once, then reveal only the panel. Animating width makes the neighbouring
 * conversation rewrap and run its ResizeObserver/scroll anchoring on every animation frame. */
export function SidePanel({ open, children, className, test }: { open: boolean; children: ReactNode; className?: string; test: Record<string, string> }) {
  const [mounted, setMounted] = useState(open)
  const body = useRef<HTMLElement>(null), opener = useRef<HTMLElement | null>(null), focused = useRef(false)
  // Capture before a child such as history's search field takes focus in its mount effect.
  useLayoutEffect(() => {
    if (open) { opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null; focused.current = false }
  }, [open])
  useEffect(() => {
    if (open) { setMounted(true); return }
    // Inert may already have blurred a keyboard user to body. Restore only their opener, not focus
    // elsewhere (including the other panel's toggle during a mutual switch).
    if (focused.current && (document.activeElement === document.body || body.current?.contains(document.activeElement))) opener.current?.focus({ preventScroll: true })
    let live = true
    // Includes reduced motion, a canceled animation (e.g. a mutual panel switch), and no animation at all.
    // An old exit must not unmount a panel that was reopened in the meantime.
    const animations = body.current?.getAnimations() ?? []
    void Promise.all(animations.map((a) => a.finished.catch(() => {}))).then(() => { if (live) setMounted(false) })
    return () => { live = false }
  }, [open])
  if (!open && !mounted) return null
  return (
    <div className={cn("flex shrink-0 justify-end overflow-hidden", PANEL_W)} inert={!open || undefined} data-side-panel data-state={open ? "open" : "closed"}>
      <aside ref={body} onFocusCapture={() => { focused.current = true }} className={cn("side-panel-body flex shrink-0 flex-col overflow-hidden border-l border-sidebar-border bg-sidebar", PANEL_W, className)} {...(open ? test : {})}>{children}</aside>
    </div>
  )
}
/** the head of a side panel (or the sheet that stands in for it on a phone): one height with the page's bar, the
 *  panel's name, a close */
export function PanelHead({ title, onClose, closeLabel, closeTest, children, className, ...rest }: { title: ReactNode; onClose: () => void; closeLabel: string; closeTest?: string; children?: ReactNode; className?: string } & Record<`data-${string}`, unknown>) {
  return (
    <div className={cn("flex h-13 shrink-0 items-center gap-2 border-b border-sidebar-border px-4", className)} {...rest}>
      <span className="min-w-0 truncate type-headline">{title}</span>
      {children}
      <Button variant="ghost" size="icon-sm" className="ml-auto text-muted-foreground" aria-label={closeLabel} onClick={onClose} data-panel-close {...(closeTest ? { [closeTest]: "" } : {})}><XIcon /></Button>
    </div>
  )
}

/* ───────── page frame: large title that collapses into the bar on scroll ───────── */
export function Page({ title, sub, back, actions, toolbar, toolbarTop = false, children, footer, max = 760, bar, noTitle = false, scrollRef, small = false, lead, center = false, virtual = false }: {
  /** a reading page that is only its column (a conversation): the column centred in the room it has, not on the
   *  frame's left edge */
  center?: boolean
  virtual?: boolean
  /** beside the title and its description (an avatar, a large icon): only in the title block, never in the bar */
  lead?: ReactNode
  /** pin the toolbar to the title line instead of the bottom of the title block */
  toolbarTop?: boolean
  small?: boolean; title: ReactNode; sub?: ReactNode; back?: [string, string]; actions?: ReactNode; toolbar?: ReactNode; children: ReactNode; footer?: ReactNode; max?: number; bar?: ReactNode; noTitle?: boolean; scrollRef?: React.RefObject<HTMLDivElement | null>
}) {
  const t = useT()
  const [scrolled, setScrolled] = useState(false)
  const own = useRef<HTMLDivElement>(null)
  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* the page's bar (App Shell Flat): the way back up as a breadcrumb, whose last crumb (this page) comes in once the
        * large title has scrolled away; a hairline under it then. The sidebar's own button folds the sidebar */}
      <header className={cn("relative z-10 flex h-12 shrink-0 items-center gap-2 px-3 transition-[border-color,background-color] duration-180 sm:px-5", scrolled || noTitle ? "border-b bg-background/80 backdrop-blur-xl" : "border-b border-transparent")} data-page-bar>
        {back ? (
          /* beside a custom bar the way back keeps its width and the bar truncates; it used to slide under the bar */
          <Breadcrumb aria-label={t("core:sidebar.breadcrumb")} moreLabel={(count) => t("core:sidebar.crumbsMore", { count })} menuLabel={t("core:sidebar.crumbsMenu")} separator="chevron" currentLast={!bar && !!textOf(title)} data-quiet={scrolled || noTitle ? undefined : ""}
            className={cn("steve-crumbs type-callout", bar ? "shrink-0" : "min-w-0")}
            items={[{ label: back[1], href: back[0], icon: <ChevronLeftIcon className="size-4" />, data: { "data-back": "" } }, ...(!bar && textOf(title) ? [{ label: textOf(title) }] : [])]} />
        ) : <MineButton className="-ml-0.5" />}
        {bar ?? (!back && <span className={cn("min-w-0 truncate type-headline transition-opacity duration-180", scrolled || noTitle ? "opacity-100" : "opacity-0")}>{title}</span>)}
        <span className="flex-1" />
        {/* on a phone the sidebar is away: the running-status pill sits in the page's header instead */}
        <span className="md:hidden"><RunningNowPill className="steve-rn-head" align="end" quiet /></span>
        <div className="flex shrink-0 items-center gap-1.5">{actions}</div>
      </header>
      <PageScroll scrollRef={scrollRef ?? own} onScroll={(e) => setScrolled(e.currentTarget.scrollTop > 36)} virtual={virtual}>
        {/* one frame for every page (1040, or 1280 for wide ones) so a title is always in the same place; a reading
          * page keeps its narrower column on the frame's left edge instead of centring a width of its own */}
        <div className={cn("mx-auto w-full px-4 sm:px-8", footer ? "pb-6" : "pb-24 md:pb-12")} style={{ maxWidth: frameOf(max) + 64 }}>
          <div className={cn(center && "mx-auto")} style={{ maxWidth: max <= 820 ? READING : undefined }}>
          {/* the title block keeps 20rem before the toolbar beside it wraps below, so a phone never squeezes the description */}
          {!noTitle && (
            <div className="flex flex-wrap items-end gap-x-4 gap-y-3 pb-8 pt-3">
              <div className={cn("min-w-[min(100%,20rem)] flex-1", lead != null && "flex items-start gap-3 sm:gap-4")}>{lead != null && <div className="shrink-0" data-page-lead>{lead}</div>}<div className="min-w-0 flex-1"><h1 className={cn(small ? "type-title" : "type-display", "text-foreground")}>{title}</h1>{sub && <div className="mt-1.5 type-callout text-muted-foreground">{sub}</div>}</div></div>
              {toolbar && <div className={cn("flex flex-wrap items-center gap-2", toolbarTop && "self-start")}>{toolbar}</div>}
            </div>
          )}
          {children}
          </div>
        </div>
      </PageScroll>
      {footer}
    </div>
  )
}
function PageScroll({ scrollRef, onScroll, virtual, children }: {
  scrollRef: React.RefObject<HTMLDivElement | null>
  onScroll: React.UIEventHandler<HTMLDivElement>
  virtual: boolean
  children: ReactNode
}) {
  return virtual
    ? <ScrollArea viewportRef={scrollRef} onScroll={onScroll} className="min-h-0 flex-1">{children}</ScrollArea>
    : <div ref={scrollRef} onScroll={onScroll} className="min-h-0 flex-1 overflow-y-auto">{children}</div>
}
/** the words of a title that may be markup (an item's number and name in spans): what a breadcrumb names it by */
export function textOf(n: ReactNode): string {
  if (n == null || typeof n === "boolean") return ""
  if (typeof n === "string" || typeof n === "number") return String(n)
  if (Array.isArray(n)) return n.map(textOf).filter(Boolean).join(" ")
  const el = n as { props?: { children?: ReactNode } }
  return el.props ? textOf(el.props.children).replace(/\s+/g, " ").trim() : ""
}
/** the page widths: reading (conversation-like and settings pages), standard, wide (stats, an item with its side column) */
const READING = 760
const frameOf = (max: number) => (max > 1080 ? 1280 : 1040)

/** what a hand-drawn floating layer shows: the value while there is one, then the last one for `ms` more with `leaving`
 *  set, so the layer can play its exit (.anim-float[data-leaving]) before it unmounts */
export function useLinger<T>(value: T | null | undefined | false, ms = 120): [T | null, boolean] {
  const last = useRef<T | null>(null), [, bump] = useState(0)
  const on = value != null && value !== false
  if (on) last.current = value as T
  useEffect(() => {
    if (on) return
    const h = setTimeout(() => { last.current = null; bump((n) => n + 1) }, ms)
    return () => clearTimeout(h)
  }, [on, ms])
  return [on ? (value as T) : last.current, !on && last.current != null]
}
/** which ends of a scroll area have more beyond them ("start", "end", both or neither), for `data-more` and a fade
 *  on that edge, so content that runs on reads as scrolled rather than cut off */
export function useScrollMore<T extends HTMLElement>(axis: "x" | "y") {
  const ref = useRef<T>(null), [more, setMore] = useState<string | undefined>()
  useEffect(() => {
    const el = ref.current
    if (!el) return
    const look = () => {
      const [at, size, all] = axis === "y" ? [el.scrollTop, el.clientHeight, el.scrollHeight] : [Math.abs(el.scrollLeft), el.clientWidth, el.scrollWidth]
      setMore([at > 1 && "start", at + size < all - 1 && "end"].filter(Boolean).join(" ") || undefined)
    }
    const ro = new ResizeObserver(look)
    ro.observe(el); for (const c of el.children) ro.observe(c)
    el.addEventListener("scroll", look, { passive: true }); look()
    return () => { ro.disconnect(); el.removeEventListener("scroll", look) }
  }, [axis])
  return [ref, more] as const
}
/* ───────── modal state ───────── */
type SheetState = { kind: string; payload?: any } | null
let sheet: SheetState = null
export const openSheet = (kind: string, payload?: any) => { sheet = { kind, payload }; emit() }
export const closeSheet = () => { sheet = null; emit() }
export const getSheet = () => sheet
export function useCommandShortcut(enabled: boolean) {
  useEffect(() => {
    if (!enabled) return
    const keydown = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== "k") return
      event.preventDefault()
      if (getSheet()?.kind === "command") closeSheet()
      else openSheet("command")
    }
    window.addEventListener("keydown", keydown)
    return () => window.removeEventListener("keydown", keydown)
  }, [enabled])
}

export function ItemRef({ id }: { id: string }) {
  const i = getItem(id)
  if (!i) return null
  return (
    <UIItem variant="outline" size="sm" render={<button type="button" onClick={() => go("#/items/" + id)} />} data-item-ref={id} className="mt-3 w-full max-w-md rounded-xl border-transparent bg-card text-left shadow-[var(--lift)] hover:bg-card hover:shadow-[var(--lift-hover)] anim-in">
      <ItemMedia variant="icon" className="rounded-lg bg-muted text-muted-foreground"><ListTodoIcon className="size-4" strokeWidth={1.5} /></ItemMedia>
      <ItemContent className="min-w-0 gap-0.5">
        <ItemTitle className="w-full min-w-0 type-body"><span className="min-w-0 truncate">{i.t}</span></ItemTitle>
        <ItemDescription className="truncate type-footnote"><span className="font-mono tabular-nums">{i.id}</span> · {botName(i.b)} · {i.progress}</ItemDescription>
      </ItemContent>
      <ItemActions><Status s={itemDisplayStatus(i)} /></ItemActions>
    </UIItem>
  )
}
