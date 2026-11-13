/* Shared building blocks for the Steve prototype — see DESIGN.md.
 * Quiet chrome: grey scale everywhere, blue = selected/running, orange = needs you. */
import { Children, Fragment, Suspense, lazy, memo, useEffect, useId, useRef, useState, useSyncExternalStore, type ComponentProps, type InputHTMLAttributes, type ReactNode, type Ref } from "react"
import { flushSync } from "react-dom"
import { Robot } from "@/robot"
import { clockDur } from "@/i18n/format"
import { LANGS, lang, setLang, t, useT, type Lang } from "@/i18n"
import { cn } from "cn"
import { itemDisplayStatus } from "@/item-workflow-relation"
import { ChevronLeftIcon, FileTextIcon, GitForkIcon, InboxIcon, ListTodoIcon, MoonIcon, PencilIcon, SearchIcon, SquareTerminalIcon, SunIcon, TerminalIcon, ArrowUpRightIcon, CornerDownRightIcon, MonitorIcon, XIcon, CircleCheckIcon, CircleSlashIcon, ChevronDownIcon, ChevronRightIcon, CircleAlertIcon, InfoIcon, TriangleAlertIcon, FileDiffIcon, FilePlusIcon, GlobeIcon, SparklesIcon, WrenchIcon,
  CircleAlertIcon as RelFailIcon, CircleCheckIcon as RelOkIcon, MegaphoneIcon as RelRunIcon,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { useFieldLabels } from "@/components/ui/input"
import { TextField as AffectTextField } from "@/components/affect/text-field-clean"
import { Trail, SubSteps, type TrailItem, type Chip } from "@/components/work-trail"
import { ThinkingLoader } from "@/components/affect/thinking-loader"
import { Item as UIItem, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemSeparator, ItemTitle } from "@/components/ui/item"
import { Empty as UIEmpty } from "@/components/ui/empty"
import { Card as AffectCard } from "@/components/affect/card-clean"
import { Card as UICard, CardAction, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card"
import { Breadcrumb } from "@/components/affect/breadcrumb-clean"
import { Bubble, BubbleContent } from "@/components/ui/bubble"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { SegmentedControl } from "@/components/affect/segmented-control-clean"
import { CodeBlock } from "@/components/ui/code-block"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { Spinner } from "@/components/ui/spinner"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Markdown } from "@/components/markdown"
import { PermissionRecord } from "@/components/permission"
import { permissionDecision, useStore } from "@/store"
import { st, emit, briefHead, ST, SS, WT, tagCatLabel, tagLabel, tagDesc, botName, decide, tag, isKind, sortTags, loadEvents, type Item, type Reply, type Rel, type Sess } from "@/store"
import { YoloBadge } from "@/yolo/badges"
import "@/themes" /* the appearance chosen on this device, on <html> before the first paint */
import { RunningNowPill } from "@/running-pill"
import { Files } from "@/attach"
import { MemConfirm, memAsk } from "@/knowledge/parts"
import { FLOW_SUBJECTS, FlowDecisionBody } from "@/auto/decision"
import { MODULE_CARDS } from "@/vault/decisions"
import { MailDraftCard } from "@/mail/draft"
import { CardTools, InlineDecision, isToolCall, mapDecision, ToolApproval } from "@/thread/decide"
import { isPeopleDecision, PeopleDecisionBody } from "@/people/pending"
import { useTurnUsage } from "@/session/turn-usage"
import { fmtEst, fmtN } from "@/stats/format"
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
  return <Segmented className={className} label={t("common:settings.language")} value={lang()} onChange={(v: Lang) => { setLang(v); emit() }} size="sm"
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
/** a panel beside a page that slides open and shut: its width eases between 0 and PANEL_W, the content keeps its
 *  width (so it doesn't reflow while it moves) and leaves the page once closed */
export function SidePanel({ open, children, className, test }: { open: boolean; children: ReactNode; className?: string; test: Record<string, string> }) {
  const [mounted, setMounted] = useState(open), [shown, setShown] = useState(open)
  useEffect(() => {
    if (open) {
      setMounted(true)
      let r = requestAnimationFrame(() => { r = requestAnimationFrame(() => setShown(true)) })
      return () => cancelAnimationFrame(r)
    }
    setShown(false)
    const h = setTimeout(() => setMounted(false), 280)
    return () => clearTimeout(h)
  }, [open])
  if (!mounted) return null
  return (
    <div className={cn("flex shrink-0 justify-end overflow-hidden transition-[width] duration-260 ease-(--ease-out)", shown && open ? PANEL_W : "w-0")} inert={!open || undefined} data-side-panel>
      <aside className={cn("flex shrink-0 flex-col overflow-hidden border-l border-sidebar-border bg-sidebar transition-opacity duration-260 ease-(--ease-out)", PANEL_W, shown && open ? "opacity-100" : "opacity-60", className)} {...(open ? test : {})}>{children}</aside>
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
export function Page({ title, sub, back, actions, toolbar, toolbarTop = false, children, footer, max = 760, bar, noTitle = false, scrollRef, small = false, lead, center = false }: {
  /** a reading page that is only its column (a conversation): the column centred in the room it has, not on the
   *  frame's left edge */
  center?: boolean
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
        ) : null}
        {bar ?? (!back && <span className={cn("min-w-0 truncate type-headline transition-opacity duration-180", scrolled || noTitle ? "opacity-100" : "opacity-0")}>{title}</span>)}
        <span className="flex-1" />
        {/* on a phone the sidebar is away: the running-status pill sits in the page's header instead */}
        <span className="md:hidden"><RunningNowPill className="steve-rn-head" align="end" quiet /></span>
        <div className="flex shrink-0 items-center gap-1.5">{actions}</div>
      </header>
      <div ref={scrollRef ?? own} onScroll={(e) => setScrolled(e.currentTarget.scrollTop > 36)} className="min-h-0 flex-1 overflow-y-auto">
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
      </div>
      {footer}
    </div>
  )
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

/* ───────── composer: its own module (composer.tsx), still reachable from here ───────── */
export { COMMANDS, Composer, markText, type ComposerHandle, type ComposerProps, type SendHow } from "@/composer"

/* ───────── transcript ─────────
 * read like a coding agent's own app: each turn is the message, a work block (thinking, commands, reads, edits —
 * open while it runs, folded to one line once done), the answer, and the files the turn changed */
const base = (p: string) => String(p || "").split("/").filter(Boolean).pop() ?? ""
export const fmtDur = (ms: number) => clockDur(ms)
const clock = (ms: number) => { const s = Math.max(0, Math.floor(ms / 1000)); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}` }
/* one clock for every running timer on the page: two views of the same run tick together and never disagree */
const ticking = new Set<(n: number) => void>()
let tickAt = 0, tickTimer: ReturnType<typeof setInterval> | undefined
/** the current time, ticking every second while `on` */
export function useNow(on: boolean) {
  const [n, setN] = useState(Date.now)
  useEffect(() => {
    if (!on) return
    if (!tickTimer) { tickAt = Date.now(); tickTimer = setInterval(() => { tickAt = Date.now(); ticking.forEach((f) => f(tickAt)) }, 1000) }
    setN(tickAt); ticking.add(setN)
    return () => { ticking.delete(setN); if (!ticking.size) { clearInterval(tickTimer); tickTimer = undefined } }
  }, [on])
  return n
}
/** a session not opened on its Node yet: how long it has been starting, and past two minutes that this is long.
 * Nothing else on the page says a start stands still: there is no turn, no event and no error to show. */
const SLOW_START_MS = 120_000
export function StartingLine({ since }: { since?: number }) {
  const t = useT(), now = useNow(true), waited = since ? now - since : 0, slow = waited >= SLOW_START_MS
  return <div className="type-callout text-muted-foreground" data-starting={slow ? "slow" : "ok"}>
    {t("core:transcript.starting")}{waited >= 10_000 && <span className="ml-1.5 tabular-nums">{clock(waited)}</span>}
    {slow && <div className="mt-1 type-footnote text-attention" data-starting-slow>{t("core:transcript.startingSlow")}</div>}
  </div>
}
const TOOLN = ["ToolSearch", "TodoWrite", "Task", "Agent", "Skill"] as const
const toolName = (tool: string) => ((TOOLN as readonly string[]).includes(tool) ? t(`core:transcript.tool.${tool as (typeof TOOLN)[number]}`) : undefined)
type StepView = { I: any; verb: string; subj: string; mono: boolean; full?: string }
/** icon, verb and subject of one tool call: the command (with the agent's own description), the file, the pattern */
function stepView(e: any[]): StepView {
  const tool = String(e[9] ?? ""), file = e[3] && e[3] !== "Steve" ? String(e[3]) : ""
  if (e[1] === "steve") return { I: SparklesIcon, verb: e[2], subj: e[10] ?? "", mono: false }
  if (e[1] === "read") return { I: FileTextIcon, verb: t("core:transcript.read"), subj: base(file) || e[2], mono: true, full: file }
  if (e[1] === "edit") return { I: tool === "Write" ? FilePlusIcon : PencilIcon, verb: t(tool === "Write" ? "core:transcript.write" : "core:transcript.edit"), subj: base(file) || e[2], mono: true, full: file }
  if (e[1] === "search") { const web = /^Web/.test(tool); return { I: web ? GlobeIcon : SearchIcon, verb: t(web ? "core:transcript.webSearch" : "core:transcript.search"), subj: e[11] ?? e[3] ?? "", mono: !web, full: e[11] } }
  if (tool && tool !== "Bash") return { I: WrenchIcon, verb: toolName(tool) ?? tool.replace(/^mcp__[^_]+__/, ""), subj: e[10] ?? e[11] ?? "", mono: !e[10] && !!e[11] }
  const cmd = String(e[11] ?? [e[2], e[3]].filter(Boolean).join(" "))
  return { I: TerminalIcon, verb: e[10] || t("core:transcript.run"), subj: cmd.split("\n")[0], mono: true, full: cmd }
}
/** "运行 14 条命令 · 读取 3 个文件 · 编辑 2 个文件 · 思考 1 次" */
function workStats(evs: any[]) {
  const n = { run: 0, read: 0, edit: 0, search: 0, other: 0, th: 0, fail: 0 }
  const files = new Set<string>()
  for (const e of evs) {
    if (e[0] === "th") n.th++
    if (e[0] !== "tool") continue
    if (e[8] === 1) n.fail++
    const v = stepView(e)
    if (v.I === TerminalIcon) n.run++
    else if (e[1] === "read") n.read++
    else if (e[1] === "edit") { n.edit++; if (e[3]) files.add(e[3]) }
    else if (e[1] === "search") n.search++
    else n.other++
  }
  return [n.run && t("core:transcript.stats.run", { count: n.run }), n.read && t("core:transcript.stats.read", { count: n.read }), files.size && t("core:transcript.stats.changed", { count: files.size }), n.search && t("core:transcript.stats.search", { count: n.search }), n.other && t("core:transcript.stats.other", { count: n.other }), n.th && t("core:transcript.stats.thought", { count: n.th }), n.fail && t("core:transcript.stats.failed", { count: n.fail })].filter(Boolean) as string[]
}
function Detail({ text }: { text: string }) {
  const lines = String(text || "").split("\n").filter(Boolean)
  if (!lines.length) return null
  return (
    <CodeBlock code={lines.map((l) => l.replace(/^[+-] /, "")).join("\n")} copyable={false} maxHeight={320} className="mt-1 max-w-160 rounded-lg"
      marks={Object.fromEntries(lines.flatMap((l, n): [number, "added" | "removed" | "ok"][] => (l.startsWith("+") ? [[n + 1, "added"]] : l.startsWith("-") ? [[n + 1, "removed"]] : l.startsWith("✓") ? [[n + 1, "ok"]] : [])))} data-tool-detail />
  )
}
const DELEGATE = ["Task", "Agent"]
/* Steve's tools that tell or ask you something: each stays its own line, never merged */
const SAY = ["steve_update", "steve_ask", "steve_request_approval", "steve_deliver", "steve_publish"]
/** the turn's tuples as steps on the trail: thinking timed by the step after it, calls of one kind merged, the latest plan open */
function trailOf(evs: any[], ts: number[], t1: number | undefined, live: boolean): TrailItem[] {
  const lastPlan = evs.findLastIndex((e) => e[0] === "tool" && e[12]?.length)
  return evs.map((e, j): TrailItem => {
    const key = String(j)
    if (e[0] === "a") return { t: "note", key, text: e[1] }
    if (e[0] === "th") {
      const at = ts[j] || 0, next = j + 1 < evs.length ? ts[j + 1] : t1
      return { t: "think", key, text: String(e[1] ?? ""), live: live && j === evs.length - 1, t0: at || undefined, ms: at && next ? next - at : undefined }
    }
    const v = stepView(e), tool = String(e[9] ?? ""), state = e[5] === 1 ? "run" : e[8] === 1 ? "fail" : "done", web = /^Web/.test(tool)
    const [verb, group, chip]: [[string, string], string | undefined, Chip | undefined] =
      DELEGATE.includes(tool) ? [[t("core:trail.live.delegate"), t("core:trail.delegated")], undefined, undefined]
      : e[1] === "read" ? [[t("core:trail.live.read"), v.verb], "read", { label: v.subj, title: v.full, mono: true, icon: FileTextIcon }]
      : e[1] === "edit" ? [[t(tool === "Write" ? "core:trail.live.write" : "core:trail.live.edit"), v.verb], "edit", { label: v.subj, title: v.full, mono: true, icon: v.I }]
      : e[1] === "search" ? [[t(web ? "core:trail.live.webSearch" : "core:trail.live.search"), v.verb], "search", { label: v.subj, title: v.full, mono: v.mono, icon: v.I }]
      : v.I === TerminalIcon ? [e[10] ? [v.verb, v.verb] : [t("core:trail.live.run"), v.verb], "run", { label: e[10] || v.subj, title: v.full, mono: !e[10], icon: TerminalIcon }]
      : [[v.verb, v.verb], e[12]?.length || SAY.includes(tool.replace(/^mcp__steve__/, "")) ? undefined : "tool:" + tool, v.subj ? { label: v.subj, mono: v.mono } : undefined]
    const g = group?.startsWith("tool:") ? "tool" : group
    const merged = g ? (calls: number, distinct: number, running: boolean) => g === "tool" ? t("core:trail.group.tool", { name: v.verb, count: calls })
      : t(`core:trail.group.${g}${running ? "Live" : ""}` as "core:trail.group.read", { count: g === "read" || g === "edit" ? distinct : calls }) : undefined
    const plan: [string, boolean][] | undefined = e[12]?.length ? e[12] : undefined
    const diff = e[1] === "edit" && e[6] != null ? <span className="shrink-0 type-footnote font-mono tabular-nums"><span className="text-success">+{e[6]}</span>{e[7] ? <span className="ml-1 text-destructive">−{e[7]}</span> : null}</span> : undefined
    /* the call as a Tool Call Card: the tool's own name, what Steve has of its input (the command, file, query or brief)
     * and what came back, its time from the update that started it to the one that settled it ([13]) */
    const out = String(e[4] ?? "").trim().replace(/^```[a-z]*\n?/, "").replace(/\n?```$/, "").trim(), t0 = ts[j] || undefined
    const card = { name: (tool || String(e[2] ?? "")).replace(/^mcp__steve__/, "").replace(/^mcp__[^_]+__/, ""), icon: <v.I strokeWidth={1.5} />,
      args: e[1] === "steve" ? e[14] ?? (e[10] || undefined) : v.I === TerminalIcon ? v.full ?? v.subj : DELEGATE.includes(tool) ? e[10] ?? e[11] : e[11] ?? (e[3] && e[3] !== "Steve" ? e[3] : undefined),
      result: state === "fail" ? undefined : plan ? (j === lastPlan ? undefined : plan.map(([x, d]) => (d ? "✓ " : "○ ") + x).join("\n")) : out || undefined,
      resultView: plan && j !== lastPlan ? <SubSteps steps={plan} live={false} /> : e[1] === "edit" && out ? <Detail text={out} /> : undefined,
      error: state === "fail" ? out || undefined : undefined, t0, ms: e[13] && t0 && e[13] > t0 ? e[13] - t0 : undefined }
    return { t: "tool", key, state, verb, subj: DELEGATE.includes(tool) ? e[10] ?? v.subj : v.subj, mono: v.mono, full: v.full, group, merged, chip, extra: diff, card,
      below: plan && j === lastPlan ? <SubSteps steps={plan} live={live} /> : undefined, attrs: { kind: e[1] } }
  })
}
/** what the agent is doing right now, in a few words: "运行 npm test", "正在思考" */
function doing(evs: any[]) {
  const run = evs.findLast((e) => e[0] === "tool" && e[5] === 1)
  if (run) { const v = stepView(run); return v.subj ? `${v.verb} ${v.subj}` : v.verb }
  const last = evs[evs.length - 1]
  return t(!last ? "core:transcript.preparing" : last[0] === "th" ? "core:transcript.thinking" : last[0] === "a" ? "core:transcript.replying" : last[0] === "ask" ? "core:transcript.awaitingApproval" : "core:transcript.processing")
}
/* a folded block stays as the reader left it, across re-renders and remounts */
const workOpen = new Map<string, boolean>(), workView = new Map<string, "steps" | "timeline">()
/* the timeline (Trace Waterfall) is a big component: loaded the first time someone asks for it */
const TurnTrace = lazy(() => import("@/thread/turn-trace"))
function WorkBlock({ k, g, live }: { k: string; g: Seg; live: boolean }) {
  const t = useT()
  const key = k + ":" + g.idx
  const [open, setOpenS] = useState(() => workOpen.get(key) ?? live)
  useEffect(() => { if (!workOpen.has(key)) setOpenS(live) }, [live, key])
  const setOpen = (v: boolean) => { workOpen.set(key, v); setOpenS(v) }
  const [view, setViewS] = useState(() => workView.get(key) ?? "steps")
  const setView = (v: "steps" | "timeline") => { workView.set(key, v); setViewS(v) }
  const now = useNow(live)
  const stats = workStats(g.evs)
  const ms = live ? now - (g.t0 ?? now) : (g.t1 ?? 0) - (g.t0 ?? 0)
  const cancelled = g.end === "cancelled"
  const items = trailOf(g.evs, g.at ?? [], g.t1, live)
  /* a finished turn of an item's session: its tokens and estimated cost, as the Hub's stats keep them */
  const usage = useTurnUsage(k, g.t0, g.t1, live || !g.end)
  return (
    <Collapsible open={open} onOpenChange={setOpen} data-work={live ? "live" : "done"} data-work-view={open ? view : undefined} className="relative anim-in">
      <CollapsibleTrigger className={cn("group/work flex w-full min-w-0 items-center gap-1.5 rounded-lg py-1 text-left type-callout hover:[&>svg]:text-foreground", open && items.length > 0 && "sm:pr-36")}>
        <ChevronDownIcon className="size-4 shrink-0 -rotate-90 text-muted-foreground transition-transform duration-180 ease-(--ease-out) group-aria-expanded/work:rotate-0" />
        <span className={cn("shrink-0", live ? "text-foreground" : "text-foreground/85")}>{live ? <span className="shimmer">{t("core:transcript.working")}</span> : t(cancelled ? "core:transcript.interrupted" : "core:transcript.handled")}</span>
        {(ms > 0 || live) && <span className="shrink-0 tabular-nums text-muted-foreground" data-work-clock>{live ? clock(ms) : fmtDur(ms)}</span>}
        {stats.length > 0 && <span className="min-w-0 truncate text-muted-foreground">· {stats.join(" · ")}</span>}
        {usage && usage.total > 0 && <span className="ml-auto hidden shrink-0 pl-2 type-footnote tabular-nums text-tertiary sm:inline" data-work-usage>{t("views:sessionPanel.turnUsage", { tokens: fmtN(usage.total), usd: fmtEst(usage.est, usage.unpriced_tokens) })}</span>}
      </CollapsibleTrigger>
      {/* open, the work can be read as steps or on a timeline; the switch sits on the header's line, beside the trigger */}
      {open && items.length > 0 && <Segmented size="sm" value={view} onChange={setView} label={t("core:trace.view.label")} className="absolute right-0 top-0 z-1 max-sm:hidden"
        options={[["steps", t("core:trace.view.steps")], ["timeline", t("core:trace.view.timeline")]] as const} />}
      <CollapsibleContent>
        {open && items.length > 0 && <Segmented size="sm" value={view} onChange={setView} label={t("core:trace.view.label")} className="mt-1 sm:hidden"
          options={[["steps", t("core:trace.view.steps")], ["timeline", t("core:trace.view.timeline")]] as const} />}
        <div className="mt-1 flex flex-col">
          {view === "timeline"
            ? <Suspense fallback={<div className="flex items-center gap-2 py-2 type-callout text-muted-foreground"><Spinner className="size-3.5" />{t("common:state.loading")}</div>}><TurnTrace k={k} evs={g.evs} at={g.at ?? []} items={items} t0={g.t0} t1={g.t1} live={live} cancelled={cancelled} /></Suspense>
            : <Trail items={items} live={live} tail={live ? 12 : undefined} end={live || !g.end ? undefined : cancelled ? { s: "cancel", label: t("core:transcript.interrupted") } : { s: "end", label: t("core:trail.finished") }} />}
        </div>
      </CollapsibleContent>
    </Collapsible>
  )
}
/** the files a turn changed, like a review summary */
function FilesCard({ files }: { files: [string, number, number][] }) {
  const t = useT()
  const add = files.reduce((n, f) => n + f[1], 0), del = files.reduce((n, f) => n + f[2], 0)
  return (
    <div className="max-w-160 overflow-hidden rounded-xl bg-card shadow-[var(--lift)] anim-in" data-files-changed>
      <div className="flex items-center gap-2 px-4 pt-3 pb-1.5 type-callout"><FileDiffIcon className="size-4 text-muted-foreground" strokeWidth={1.75} /><span className="font-medium text-foreground">{t("core:transcript.filesChanged", { count: files.length })}</span><span className="type-footnote font-mono tabular-nums"><span className={add ? "text-success" : "text-tertiary"}>+{add}</span> <span className={del ? "text-destructive" : "text-tertiary"}>−{del}</span></span></div>
      <ul className="flex flex-col pb-2.5">{files.slice(0, 12).map(([p, a, d]) => (
        <li key={p} className="flex min-w-0 items-center gap-2 px-4 py-0.5 type-footnote font-mono" title={p}><span className="shrink-0 text-foreground">{base(p)}</span><span className="min-w-0 truncate text-muted-foreground">{p.split("/").slice(-4, -1).join("/")}</span><span className="ml-auto shrink-0 tabular-nums"><span className={a ? "text-success" : "text-tertiary"}>+{a}</span> <span className={d ? "text-destructive" : "text-tertiary"}>−{d}</span></span></li>
      ))}{files.length > 12 && <li className="px-4 py-0.5 type-footnote text-muted-foreground">{t("core:transcript.moreFiles", { count: files.length - 12 })}</li>}</ul>
    </div>
  )
}
/* U10: a session whose Node events were set aside never recovers by itself; the way forward is offered right there */
function FaultedLine({ k, text }: { k: string; text: string }) {
  const t = useT()
  const [busy, setBusy] = useState(false)
  const act = (f: (api: typeof import("@/api").api) => Promise<void>) => {
    setBusy(true)
    void import("@/api").then(m => f(m.api)).catch((e: Error) => { void import("@/store").then(s => s.toast(e.message)) }).finally(() => setBusy(false))
  }
  return (
    <div className="flex flex-col items-center gap-2 type-footnote text-muted-foreground" data-faulted>
      <span className="max-w-[80%] text-center">{text}</span>
      <div className="flex gap-2">
        <Button size="xs" variant="outline" disabled={busy} data-continue-new onClick={() => act(async api => { const r = await api.continueSession(k); go("#/sessions/" + r.session) })}>{t("core:transcript.continueNew")}</Button>
        <Button size="xs" variant="ghost" disabled={busy} data-close-session onClick={() => act(async api => { await api.endSession(k) })}>{t("core:transcript.closeSession")}</Button>
      </div>
    </div>
  )
}

function ForkBtn({ k, at }: { k: string; at: number }) {
  const t = useT()
  return (
    <Tooltip>
      <TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("core:transcript.fork")} data-fork={at} onClick={() => openSheet("fork", { k, at })}
        className="shrink-0 rounded-full text-muted-foreground opacity-0 transition-opacity duration-120 group-hover/msg:opacity-100 focus-visible:opacity-100 [@media(hover:none)]:opacity-100" />}><GitForkIcon /></TooltipTrigger>
      <TooltipContent>{t("core:transcript.fork")}</TooltipContent>
    </Tooltip>
  )
}
/* a prompt Steve hands an agent is not a message: the task brief (role, rules, retrieved knowledge) folds to one line, its text kept verbatim */
export function Brief({ text, label, className }: { text: string; label?: string; className?: string }) {
  const head = briefHead(text), who = label && <span className="mr-1.5 font-medium text-foreground/80">{label}</span>
  if (head === text.trim()) return <div className={cn("break-words", className)}>{who}{text}</div>
  return (
    <Collapsible className={cn("min-w-0", className)} data-brief>
      <CollapsibleTrigger className="group/brief flex max-w-full items-center gap-1 text-left hover:text-foreground">
        <span className="min-w-0 truncate">{who}{head}</span><ChevronRightIcon className="size-3.5 shrink-0 transition-transform duration-180 ease-(--ease-out) group-data-[panel-open]/brief:rotate-90" />
      </CollapsibleTrigger>
      <CollapsibleContent><div className="mt-1.5 max-h-80 overflow-auto break-words border-l-2 border-border py-0.5 pl-3 text-muted-foreground" data-brief-body><Markdown text={text} className="type-footnote" /></div></CollapsibleContent>
    </Collapsible>
  )
}
/** range = [from, to) into the session's tuples: only that part of the conversation (one reply in the work log) */
type Seg = { t: string; evs: any[]; idx: number; /** when each of `evs` came */ at?: number[]; t0?: number; t1?: number; end?: string; files?: [string, number, number][] }
/* one segment of a transcript: while a reply streams, only the segment that grew renders again */
const sameEvs = (a: any[], b: any[]) => a.length === b.length && a.every((x, i) => x === b[i] || (x.length === b[i].length && x.every((v: any, j: number) => v === b[i][j])))
function TranscriptPermission({ e }: { e: any[] }) {
  useStore()
  return <PermissionRecord title={e[1]} id={e[2]} decision={permissionDecision(e[2])} />
}
const SegView = memo(function SegView({ k, g, live }: { k: string; g: Seg; live: boolean }) {
  const t = useT()
  const e = g.evs[0]
  if (g.t === "work") return <WorkBlock k={k} g={g} live={live} />
  if (g.t === "files") return <FilesCard files={g.files!} />
  if (g.t === "u") return (
    <div className="group/msg flex items-center justify-end gap-1 pl-12 anim-in" data-user-msg>
      <ForkBtn k={k} at={g.idx} />
      <div className="flex max-w-[85%] flex-col items-end gap-1.5">
        <Files files={e[3]} align="end" />
        {(e[1] !== "（附件）" || !e[3]?.length) && <Bubble variant="muted" align="end" className="max-w-full">
          <BubbleContent className="min-w-0 break-words rounded-2xl px-4 py-2.5 type-body text-foreground">{e[2] && <div className="mb-0.5 type-label text-muted-foreground">{e[2]}</div>}<Markdown text={e[1]} /></BubbleContent>
        </Bubble>}
      </div>
    </div>
  )
  if (g.t === "a") return (
    <div className="group/msg flex items-start gap-1 anim-in" data-agent-msg>
      <div className="min-w-0 flex-1 break-words type-body text-foreground"><Markdown text={e[1]} /></div>
      <ForkBtn k={k} at={g.idx} />
    </div>
  )
  if (g.t === "q") return (
    <div className="ml-auto max-w-[85%] rounded-2xl bg-muted px-4 py-2.5 anim-in" data-merged>
      <div className="mb-1 flex items-center gap-1.5 type-label text-muted-foreground"><InboxIcon className="size-3.5" />{t("core:transcript.merged", { count: e[1].length })}</div>
      {e[1].map((x: string) => <Markdown key={x} text={x} className="type-body" />)}
    </div>
  )
  if (g.t === "hi") return <div className="flex gap-2.5 rounded-xl bg-muted/60 px-3.5 py-2.5 type-callout text-muted-foreground anim-in" data-handoff><CornerDownRightIcon className="mt-0.5 size-4 shrink-0" strokeWidth={1.5} /><div className="min-w-0 flex-1"><Brief text={e[3] ?? e[1]} label={t("core:transcript.steveBrief")} /><Files files={e[2]} className="mt-1.5" /></div></div>
  if (g.t === "ho") return (
    <div className="flex items-center gap-3 type-footnote text-muted-foreground"><span className="h-px flex-1 bg-border" />
      <button type="button" onClick={() => go("#/sessions/" + e[2])} className="inline-flex items-center gap-1 text-blue hover:underline">{e[1]}<ArrowUpRightIcon className="size-3" /></button><span className="h-px flex-1 bg-border" /></div>
  )
  if (g.t === "ask") return <TranscriptPermission e={e} />
  if (g.t === "away") return <div className="flex items-center gap-2 type-footnote text-muted-foreground" data-away><CornerDownRightIcon className="size-3.5 shrink-0" strokeWidth={1.5} />{t("core:transcript.away")}</div>
  if (g.t === "sys" && e[2] === "faulted") return <FaultedLine k={k} text={e[1]} />
  if (g.t === "sys") return <SysRow text={String(e[1] ?? "")} />
  return null
}, (a, b) => a.k === b.k && a.live === b.live && a.g.idx === b.g.idx && a.g.t === b.g.t && a.g.t1 === b.g.t1 && a.g.end === b.g.end && a.g.files?.length === b.g.files?.length && sameEvs(a.g.evs, b.g.evs))
/** an event in the conversation (started, queued, compacted…): a short one is a centred line between rules, truncated
 *  with the whole text on hover; a longer notice is a left-aligned muted fill with an icon, two lines at most */
export function SysRow({ text, className }: { text: string; className?: string }) {
  if (text.length <= 40 && !text.includes("\n")) return (
    <div className={cn("flex items-center gap-3 type-footnote text-muted-foreground", className)} data-sys="line"><span className="h-px flex-1 bg-border/70" /><span className="min-w-0 max-w-[80%] truncate text-center" title={text}>{text}</span><span className="h-px flex-1 bg-border/70" /></div>
  )
  return (
    <div className={cn("flex items-start gap-2 rounded-lg bg-muted/60 px-3 py-2 type-footnote text-muted-foreground", className)} title={text} data-sys="notice">
      <InfoIcon className="mt-0.5 size-3.5 shrink-0" strokeWidth={1.75} aria-hidden /><span className="min-w-0 flex-1 line-clamp-2 break-words">{text}</span>
    </div>
  )
}
const PROMPT = ["u", "hi", "q"]
/** when the turn holding tuple `at` started: its prompt's time; 0 when no prompt opened it (the work's first step stands in) */
function turnStart(s: Sess, at: number) {
  for (let i = at - 1; i >= 0; i--) {
    if (PROMPT.includes(s.ev[i][0])) return s.ts?.[i] ?? 0
    if (s.ev[i][0] === "end") return 0
  }
  return 0
}
/** tuples → turns: the message, the work (folded), the answer, the files changed.
 * `skip`: turns shown elsewhere on the page (a comment's reply in the item feed) — one pointer stands in for each */
function segsOf(s: Sess, range?: [number, number], skip?: [number, number][]): Seg[] {
  const segs: Seg[] = [], ts = s.ts ?? []
  /* a range cut after its prompt still times from the prompt, like the whole transcript and the run bar do */
  let buf: [any[], number][] = [], t0 = range ? turnStart(s, range[0]) : 0, work: Seg | null = null, files = new Map<string, [string, number, number]>()
  const flush = (end?: [string, number]) => {
    if (!buf.length) return
    const ans = buf[buf.length - 1][0][0] === "a" ? buf.pop()! : null
    if (buf.length) {
      const lastIdx = buf[buf.length - 1][1]
      segs.push(work = { t: "work", evs: buf.map((x) => x[0]), at: buf.map((x) => ts[x[1]] ?? 0), idx: buf[0][1], t0: t0 || ts[buf[0][1]], t1: end?.[1] ?? ts[lastIdx + 1] ?? ts[lastIdx], end: end?.[0] })
      for (const [e] of buf) if (e[0] === "tool" && e[1] === "edit" && e[6] != null && e[8] !== 1 && e[3]) { const f = files.get(e[3]) ?? [e[3], 0, 0]; f[1] += e[6]; f[2] += e[7] ?? 0; files.set(e[3], f) }
    }
    if (ans) segs.push({ t: "a", evs: [ans[0]], idx: ans[1] })
    buf = []
  }
  const turnDone = (idx: number) => { if (files.size) segs.push({ t: "files", evs: [], idx, files: [...files.values()] }); files = new Map() }
  s.ev.forEach((e, idx) => {
    if (range && (idx < range[0] || idx >= range[1])) return
    const away = skip?.find(([a, b]) => idx >= a && idx < b)
    if (away) { if (idx === away[0]) { flush(); turnDone(idx); segs.push({ t: "away", evs: [], idx }); t0 = 0; work = null } return }
    if (e[0] === "th" || e[0] === "tool" || e[0] === "a") { buf.push([e, idx]); return }
    if (e[0] === "end") {
      /* the turn's work may already be closed (a line from the Hub came before its end): the end still belongs to it */
      if (!buf.length && work && !work.end) Object.assign(work, { end: e[1], t1: ts[idx] })
      flush([e[1], ts[idx]]); turnDone(idx); t0 = 0; work = null; return
    }
    if (PROMPT.includes(e[0])) { flush(); turnDone(idx); segs.push({ t: e[0], evs: [e], idx }); t0 = ts[idx] ?? 0; work = null; return }
    /* a line inside the turn (a permission asked, a message queued) splits the work: what follows times from its own first step */
    const split = buf.length > 0
    flush(); segs.push({ t: e[0], evs: [e], idx }); if (split) t0 = 0
  })
  flush()
  if (s.st !== "run") turnDone(s.ev.length)
  return segs
}
/** a session's work, turn by turn, with the steps Steve shows for it: what the call tree of the collaboration draws
 * (collab-graph.tsx); the live one is the running session's last, unfinished work */
export function sessionWork(k: string): { evs: any[]; at: number[]; items: TrailItem[]; t0: number; t1?: number; live: boolean; cancelled: boolean }[] {
  const s = st.S[k]
  if (!s?.loaded) return []
  const segs = segsOf(s), last = segs[segs.length - 1]
  return segs.filter((g) => g.t === "work").map((g) => {
    const live = s.st === "run" && g === last && !g.end
    return { evs: g.evs, at: g.at ?? [], items: trailOf(g.evs, g.at ?? [], g.t1, live), t0: g.t0 || (g.at ?? []).find(Boolean) || 0, t1: g.t1, live, cancelled: g.end === "cancelled" }
  })
}
type TP = { k: string; compact?: boolean; tail?: number; range?: [number, number]; skip?: [number, number][] }
/* the whole app re-renders on every store change: a transcript (hundreds of steps, markdown) only when its own events or status did */
export function Transcript(p: TP) { const s = st.S[p.k]; return <TranscriptOf {...p} ev={s?.ev} st={s?.st} loaded={s?.loaded} /> }
const TranscriptOf = memo(TranscriptImpl, (a, b) => a.k === b.k && a.ev === b.ev && a.st === b.st && a.loaded === b.loaded && a.compact === b.compact && a.tail === b.tail && a.range?.[0] === b.range?.[0] && a.range?.[1] === b.range?.[1] && String(a.skip ?? "") === String(b.skip ?? ""))
function TranscriptImpl({ k, tail, range, skip }: TP & { ev?: unknown; st?: string; loaded?: boolean }) {
  const t = useT()
  const s = st.S[k]
  const [full, setFull] = useState(false)
  useEffect(() => { if (s && !s.loaded) void loadEvents(k) }, [k, s?.loaded])
  if (!s) return null
  if (!s.loaded) return <div className="flex items-center gap-2 type-callout text-muted-foreground"><Spinner className="size-3.5" />{t("core:transcript.loading")}</div>
  if (!s.ev.length && !range) return s.starting ? <StartingLine since={s.since} /> : <div className="type-callout text-muted-foreground">{t(s.st === "run" ? "core:transcript.starting" : "core:transcript.empty")}</div>
  const segs = segsOf(s, range, skip)
  /* the turn running now may be shown elsewhere (skipped): then its progress is there, not here */
  const last = segs[segs.length - 1], tailOpen = (!range || range[1] >= s.ev.length) && last?.t !== "away"
  const liveIdx = s.st === "run" && tailOpen && last?.t === "work" && !last.end ? segs.length - 1 : -1
  const from = tail && !full && segs.length > tail ? segs.length - tail : 0
  return (
    <div className="flex flex-col gap-4" data-transcript={k}>
      {from > 0 && <button type="button" onClick={() => setFull(true)} className="self-start type-footnote text-muted-foreground hover:text-foreground" data-show-all>{t("core:transcript.moreSegs", { count: from })}</button>}
      {segs.map((g, n) => n < from ? null : <SegView key={k + ":" + g.idx + ":" + g.t} k={k} g={g} live={n === liveIdx} />)}
      {s.st === "run" && liveIdx < 0 && tailOpen && <div className="flex" data-transcript-pending><ThinkingLoader variant="plain" size="sm" phases={[t(last?.t === "a" ? "core:transcript.replying" : "core:transcript.startingShort")]} phase={0} className="[--ui-tlc-accent:var(--blue)]!" /></div>}
    </div>
  )
}

/** the session's state at a glance, above the composer: running and doing what, waiting on you, or done and how it went.
 *  Only a running or waiting turn keeps its tinted box; a finished one (done, stopped, ended) is one quiet line. On a
 *  phone the bar keeps the state and the time, not the step summary. Stopping the turn is the composer's (its Stop and Esc). */
export function RunBar({ k }: { k: string }) {
  const t = useT()
  const s = st.S[k], run = s?.st === "run"
  const now = useNow(run)
  if (!s || !s.loaded || (!s.ev.length && !run)) return null
  let i = s.ev.length - 1
  while (i >= 0 && !["u", "hi", "q"].includes(s.ev[i][0])) i--
  /* before the session is up there is no turn: the clock counts the start itself */
  const starting = run && !!s.starting && !s.ev.length
  const turn = s.ev.slice(i + 1), t0 = (starting && s.since) || (i >= 0 ? s.ts[i] : s.ts[0]) || now
  const end = turn.findLast((e) => e[0] === "end"), endAt = end ? s.ts[s.ev.lastIndexOf(end)] : 0
  const stats = workStats(turn)
  const tone = run ? "run" : s.st === "wait" ? "wait" : end?.[1] === "cancelled" ? "stop" : s.st === "ended" ? "ended" : "done"
  const label = starting ? t("core:transcript.startingShort") : t(`core:runBar.${tone}`)
  const what = run ? null : tone === "wait" ? t("core:runBar.waitWhat") : stats.join(" · ")
  return (
    /* running, the Thinking Loader inside is the live region that speaks each step; the bar itself would say it twice */
    <div role={run ? undefined : "status"} aria-live={run ? undefined : "polite"} data-runbar={tone}
      className={cn("mb-2 flex items-center gap-2.5 rounded-xl type-callout transition-colors duration-180 ease-(--ease-out)",
        tone === "run" ? "min-h-10 bg-blue/8 px-3.5 py-2" : tone === "wait" ? "min-h-10 bg-attention/10 px-3.5 py-2" : "min-h-8 py-1")}>
      {/* running: a Thinking Loader whose label is the step under way, rolling over as the steps change */}
      {run ? <span className="flex min-w-0 flex-1 items-center" data-doing><ThinkingLoader variant="plain" size="sm" phases={[starting ? label : doing(turn)]} phase={0} className="min-w-0 [--ui-tlc-accent:var(--blue)]! [--ui-tlc-ink:var(--foreground)]!" /></span>
        : tone === "wait" ? <StatusDot s="wait" className="mx-1" /> : tone === "done" ? <CircleCheckIcon className="size-4 shrink-0 text-success" strokeWidth={1.75} /> : tone === "stop" ? <CircleSlashIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.75} /> : <StatusDot s="ended" className="mx-1" />}
      {!run && <span className={cn("shrink-0 font-medium", tone === "wait" ? "text-attention" : "text-foreground/85")}>{label}</span>}
      {/* plain text in its own box: a flex row cuts text off without the ellipsis */}
      {what && <span className={cn("flex min-w-0 items-baseline gap-1.5 truncate max-sm:hidden", run ? "text-foreground/85" : "text-muted-foreground")} data-doing>{typeof what === "string" ? <span className="min-w-0 truncate">{what}</span> : what}</span>}
      <span className="ml-auto shrink-0 type-footnote tabular-nums text-muted-foreground">{run ? clock(now - t0) : endAt && t0 ? t("core:runBar.took", { dur: fmtDur(endAt - t0) }) : ""}</span>
    </div>
  )
}

/* a session as a block inside the item's work log */
const expanded = new Set<string>(), collapsedLive = new Set<string>()
/** a reply as one line of plain text: the Markdown marks of its first lines dropped */
const plainLine = (md: string) => md.replace(/```[\s\S]*?```/g, " ").replace(/[`*_>#|]/g, "").replace(/\s+/g, " ").trim()
export function SessionBlock({ k, defaultOpen, replies }: { k: string; defaultOpen?: boolean; /** comments' replies the feed shows on their own */ replies?: Reply[] }) {
  const t = useT()
  const [, f] = useState(0)
  const s = st.S[k]
  if (!s) return null
  /* a turn answering a comment is shown under that comment, with its prompt; here it would be the same work a second time (ST-48) */
  const skip = replies?.filter((r) => r.s === k).map((r): [number, number] => { let a = r.from; while (a > 0 && PROMPT.includes(s.ev[a - 1]?.[0])) a--; return [a, r.to] })
  const live = s.st === "run" || s.st === "wait"
  const open = expanded.has(k) ? true : collapsedLive.has(k) ? false : (defaultOpen ?? live)
  const toggle = () => { if (open) { expanded.delete(k); collapsedLive.add(k) } else { expanded.add(k); collapsedLive.delete(k) } f((x) => x + 1) }
  const lastA = [...s.ev].reverse().find((e) => e[0] === "a")
  return (
    <Collapsible open={open} onOpenChange={(v) => { if (v !== open) toggle() }} data-session-block={k}>
      <div className="group/sess flex min-h-9 items-center gap-2">
        <span className="flex size-5 shrink-0 items-center justify-center"><SquareTerminalIcon className="size-4 text-muted-foreground" strokeWidth={1.5} /></span>
        <button type="button" onClick={toggle} className="group/toggle flex min-w-0 flex-1 items-center gap-2.5 text-left" aria-expanded={open}>
          <span className="shrink-0 type-callout font-medium">{t("core:session.no", { n: s.n })}</span>
          <Status s={s.st} sess />
          <span className="min-w-0 truncate type-footnote text-muted-foreground">{open ? `${st.devices.find((d) => d.id === s.device)?.name ?? "Hub"} · ${s.h} · ${s.m} · ${s.dur}${s.ctx ? t("core:session.context", { pct: s.ctx }) : ""}` : lastA ? plainLine(String(lastA[1])) : s.starting ? t("core:transcript.startingShort") : s.st === "run" ? t("core:runBar.run") : ""}</span>
          <span className="ml-auto shrink-0 type-footnote text-muted-foreground group-hover/toggle:text-foreground">{t(open ? "core:session.collapse" : "core:session.expand")}</span>
        </button>
        <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" onClick={() => go("#/sessions/" + k)} data-enter-session={k} aria-label={t("core:session.enter")} className="text-muted-foreground" />}><ArrowUpRightIcon /></TooltipTrigger><TooltipContent>{t("core:session.enter")}</TooltipContent></Tooltip>
      </div>
      <CollapsibleContent>
        <div className="mb-2 ml-2.5 mt-1 border-l pl-4.5"><Transcript k={k} compact={!live} tail={4} skip={skip?.length ? skip : undefined} /></div>
      </CollapsibleContent>
    </Collapsible>
  )
}

/* the agent's answer to one comment, right under the comment; opens to that turn of the session.
 * Folded, its header sums the work up; open, the work block inside does, so the header says only whose reply it is.
 * Whether it is open outlives a remount: lines written while it runs shift its place in the feed */
const replyOpen = new Set<string>()
export function ReplyBlock({ r }: { r: Reply }) {
  const t = useT()
  const key = r.s + ":" + r.from
  const [open, setOpenS] = useState(() => replyOpen.has(key))
  const setOpen = (v: boolean) => { if (v) replyOpen.add(key); else replyOpen.delete(key); setOpenS(v) }
  const s = st.S[r.s]
  if (!s) return null
  const who = s.b ?? "agent"
  if (!r.text && !r.tools) return r.live ? <div className="ml-7 flex items-center gap-2 py-1 type-footnote text-muted-foreground" data-reply-live><Spinner className="size-3" /><span className="shimmer">{t("core:session.botReplying", { bot: botName(who) })}</span></div> : null
  /* counted as the work block counts them: commands, files changed */
  const stats = open ? [] : workStats(s.ev.slice(r.from, r.to))
  return (
    <div className="mb-1.5 ml-2.5 border-l pl-4.5" data-reply={r.s}>
      <div className="flex min-h-7 min-w-0 items-center gap-1.5 type-footnote text-muted-foreground">
        <BotAvatar k={who} size={16} /><span className="shrink-0 font-medium text-foreground/80">{botName(who)}</span>
        <span className="shrink-0">{t("core:session.replyNo", { n: s.n })}</span>
        {stats.length > 0 && <span className="min-w-0 truncate" data-reply-stats>· {stats.join(" · ")}</span>}
        {r.live && !open && <Spinner className="size-3 shrink-0" />}
        <button type="button" onClick={() => setOpen(!open)} aria-expanded={open} className="ml-auto shrink-0 hover:text-foreground" data-reply-toggle>{t(open ? "core:session.collapseWork" : "core:session.expandWork")}</button>
        <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" onClick={() => go("#/sessions/" + r.s)} aria-label={t("core:session.enter")} className="text-muted-foreground" />}><ArrowUpRightIcon /></TooltipTrigger><TooltipContent>{t("core:session.enter")}</TooltipContent></Tooltip>
      </div>
      <Collapsible open={open} onOpenChange={setOpen}>
        <CollapsibleContent>
          <div className="pb-2 pt-1"><Transcript k={r.s} compact={!r.live} range={[r.from, r.to]} /></div>
        </CollapsibleContent>
      </Collapsible>
      {!open && r.text ? <div className="line-clamp-6 whitespace-pre-wrap break-words pb-1 type-body text-foreground animate-[steve-fade_180ms_var(--ease-out)_both]" data-reply-text><Markdown text={r.text} /></div> : null}
    </div>
  )
}

/* ───────── decisions ───────── */
export function DecisionCard({ i, w = i.wait!, showItem = true }: { i: Item; w?: Item["wait"] & {}; showItem?: boolean }) {
  const t = useT()
  const [localBusy, setBusy] = useState(false)
  const busy = localBusy || st.pendingActions.has("decision:" + w.decision)
  const act = async (c: string, t?: string) => { if (busy) return; setBusy(true); try { await decide(i.id, c, t, w.decision) } finally { setBusy(false) } }
  const head = (title: ReactNode) => (
    <CardHeader className="flex min-w-0 flex-col gap-2 pt-4 pb-3">
      <div className="flex w-full min-w-0 flex-wrap items-center justify-between gap-x-3 gap-y-1">
      <CardDescription className="flex min-w-0 max-w-full items-center gap-2 type-footnote">
        <StatusDot s={w.type === "propose" ? "prop" : w.type === "fail" ? "fail" : "wait"} /><span className="shrink-0 whitespace-nowrap font-medium text-foreground/80">{w.subject === "tool_call" ? t("core:wait.tool") : w.subject === "release" ? t("core:wait.release") : WT[w.type]}</span>
        {showItem && (i.id ? <><span className="text-tertiary">·</span><a href={"#/items/" + i.id} className="min-w-0 truncate hover:text-foreground hover:underline"><span className="font-mono tabular-nums">{i.id}</span> {i.t}</a></>
          : w.session ? <><span className="text-tertiary">·</span><a href={"#/sessions/" + w.session} className="min-w-0 truncate hover:text-foreground hover:underline" data-orphan-session={w.session}>{t("core:card.sessionTitle", { title: i.t })}</a></> : null)}
      </CardDescription>
      {/* who waits since when, remind me later, withdraw the card or give up the item */}
      <CardAction className="flex max-w-full items-center"><CardTools i={i} w={w} /></CardAction>
      </div>
      {title && <CardTitle className="col-span-full min-w-0 break-words type-headline">{title}</CardTitle>}
      <YoloBadge decision={w.decision} subject={w.subject} />
      {w.busy && <div role="status" className="col-span-full flex items-center gap-1.5 type-footnote text-muted-foreground" data-agent-busy><StatusDot s="run" className="size-1.5" />{t("core:card.agentBusy")}</div>}
    </CardHeader>
  )
  const foot = (c: ReactNode) => <CardFooter className="flex-wrap justify-end gap-2 bg-muted/50 px-4 py-3! dark:bg-muted/40">{c}</CardFooter>
  let body: ReactNode = null
  if (FLOW_SUBJECTS.includes(w.subject)) body = <FlowDecisionBody w={w} act={(c) => void act(c)} busy={busy} head={head} foot={foot} />
  else if (w.type === "confirm" && w.subject === "mem") body = <>{head(memAsk(w.q))}<MemConfirm q={w.q} draft={w.draft} busy={busy} act={(c, t) => void act(c, t)} /></>
  else if (isPeopleDecision(w)) body = <>{head(w.q)}<PeopleDecisionBody w={w} busy={busy} act={(c) => void act(c)} /></>
  else if (w.subject === "mail_draft") body = <MailDraftCard w={w} act={act} busy={busy} head={head} foot={foot} />
  else if (MODULE_CARDS[w.subject]) body = MODULE_CARDS[w.subject]({ w, act, busy, head, foot })
  /* everything else is affectUI's Decision Card (a tool permission its Approval Card), wired to the same answers */
  else if (isToolCall(w)) return <ToolApproval i={i} w={w} showItem={showItem} />
  else { const m = mapDecision(i, w, "inline", showItem); return m ? <InlineDecision m={m} /> : null }
  return <UICard size="sm" className="gap-0 py-0 anim-in" data-decision={w.type} data-decision-wrap={i.id || w.decision} aria-busy={busy}>{busy && <div role="status" className="px-4 py-2 type-footnote text-muted-foreground">{t("core:card.submitting")}</div>}{body}</UICard>
}

export function ItemRef({ id }: { id: string }) {
  const i = st.items.find((x) => x.id === id)
  if (!i) return null
  return (
    <UIItem variant="outline" size="sm" render={<button type="button" onClick={() => go("#/items/" + id)} />} data-item-ref={id} className="mt-3 w-full max-w-md rounded-xl border-transparent bg-card text-left shadow-[var(--lift)] hover:bg-card hover:shadow-[var(--lift-hover)] anim-in">
      <ItemMedia variant="icon" className="rounded-lg bg-muted text-muted-foreground"><ListTodoIcon className="size-4" strokeWidth={1.5} /></ItemMedia>
      <ItemContent className="min-w-0 gap-0.5">
        <ItemTitle className="w-full min-w-0 type-body"><span className="min-w-0 truncate">{i.t}</span></ItemTitle>
        <ItemDescription className="truncate type-footnote"><span className="font-mono tabular-nums">{i.id}</span> · {botName(i.b)} · {i.now}</ItemDescription>
      </ItemContent>
      <ItemActions><Status s={itemDisplayStatus(i)} /></ItemActions>
    </UIItem>
  )
}
