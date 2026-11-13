/* Shared building blocks for the Steve prototype — see DESIGN.md.
 * Quiet chrome: grey scale everywhere, blue = selected/running, orange = needs you. */
import { Children, Fragment, memo, useEffect, useId, useLayoutEffect, useRef, useState, useSyncExternalStore, type ReactNode } from "react"
import { flushSync } from "react-dom"
import { Robot } from "@/robot"
import { cue } from "@/bus"
import { clockDur } from "@/i18n/format"
import { LANGS, lang, setLang, t, useT, type Lang } from "@/i18n"
import { cn } from "cn"
import { itemDisplayStatus } from "@/item-workflow-relation"
import { BookOpenIcon, ChartNoAxesColumnIcon,
  ArrowUpIcon, CheckIcon, ChevronLeftIcon, FileTextIcon, GitForkIcon, InboxIcon, ListTodoIcon,
  MessageCircleIcon, MoonIcon, PencilIcon, RadioIcon, SearchIcon, SettingsIcon, SquareTerminalIcon,
  SunIcon, TerminalIcon, ArrowUpRightIcon, UserIcon, UserRoundIcon, ContactRoundIcon, KeyRoundIcon, UsersRoundIcon, ServerIcon, CornerDownRightIcon, MonitorIcon, WorkflowIcon, HashIcon, BotIcon, AtSignIcon, SlashIcon, XIcon, SquareIcon, AlarmClockIcon, FolderIcon, EllipsisIcon, BellOffIcon, BanIcon,
  MailIcon,
  CircleCheckIcon, CircleSlashIcon, ChevronDownIcon, ChevronRightIcon, FileDiffIcon, FilePlusIcon, GlobeIcon, SparklesIcon, WrenchIcon,
  CircleAlertIcon as RelFailIcon, CircleCheckIcon as RelOkIcon, MegaphoneIcon as RelRunIcon,
  PlugIcon, MessagesSquareIcon,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Trail, SubSteps, type TrailItem, type Chip } from "@/components/work-trail"
import { Item as UIItem, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemSeparator, ItemTitle } from "@/components/ui/item"
import { Empty as UIEmpty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty"
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card"
import { Breadcrumb, BreadcrumbItem, BreadcrumbLink, BreadcrumbList, BreadcrumbPage, BreadcrumbSeparator } from "@/components/ui/breadcrumb"
import { Command, CommandGroup, CommandItem, CommandList } from "@/components/ui/command"
import { Bubble, BubbleContent } from "@/components/ui/bubble"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { Kbd } from "@/components/ui/kbd"
import { Sidebar as UISidebar, SidebarContent, SidebarFooter, SidebarGroup, SidebarGroupLabel, SidebarGroupContent, SidebarHeader, SidebarMenu, SidebarMenuBadge, SidebarMenuButton, SidebarMenuItem, SidebarRail, SidebarTrigger } from "@/components/ui/sidebar"
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupTextarea } from "@/components/ui/input-group"
import { CodeBlock, CodeBlockContent, CodeBlockLine } from "@/components/ui/code-block"
import { Questionnaire, QuestionnaireItem, QuestionnaireTitle, QuestionnaireChoices, QuestionnaireChoice, QuestionnaireActions, QuestionnaireSubmit } from "@/components/ui/questionnaire"
import { Textarea } from "@/components/ui/textarea"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Spinner } from "@/components/ui/spinner"
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible"
import { Markdown } from "@/components/markdown"
import { PermissionDetails, PermissionRecord } from "@/components/permission"
import { LOADED_WEB_VERSION, permissionDecision, useStore } from "@/store"
import { shortVersion } from "@/updates"
import { st, emit, briefHead, pendingCards, acceptChoosesRelease, ST, SS, WT, FAIL_CHOICE, SNOOZE_KEYS, snoozeLabel, tagCatLabel, tagLabel, tagDesc, botName, decide, dismiss, setSt, snooze, tag, isKind, sortTags, loadEvents, type Item, type Reply, type Rel, type Sess } from "@/store"
import { YoloBadge } from "@/yolo/badges"
import { YoloPill } from "@/yolo/switch"
import { WorkspaceSwitcher } from "@/workspace-switcher"
import { HubSwitcher } from "@/hub-ui"
import { useUploads, PendingList, AttachButton, Files } from "@/attach"
import type { RFile } from "@/api"
import { MemConfirm, memAsk } from "@/knowledge/parts"
import { FLOW_SUBJECTS, FlowDecisionBody } from "@/auto/decision"
import { MODULE_CARDS } from "@/vault/decisions"
import { OutContext } from "@/channels/out-context"
import { MailDraftCard } from "@/mail/draft"
import { useMentions } from "@/people/mention"
import { clearSentDraft, draftToSend, readDraft, saveDraft } from "@/composer-draft"
import { isPeopleDecision, PeopleDecisionBody } from "@/people/pending"
import { usePeopleOn } from "@/api/people"

/* ───────── routing ───────── */
/* a route change cross-fades the content area (view-transition-name: page) while the sidebar stays; without view
 * transitions, or with reduced motion, it just changes */
const hashSub = (cb: () => void) => {
  const on = () => {
    const d = document as Document & { startViewTransition?: (f: () => void) => unknown }
    if (d.startViewTransition && !matchMedia("(prefers-reduced-motion: reduce)").matches) d.startViewTransition(() => flushSync(cb))
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
/** the theme as one quiet button that steps light → dark → system; its tooltip names the current one */
function ThemeCycle({ className }: { className?: string }) {
  const t = useT()
  const order: ThemePref[] = ["light", "dark", "system"], I = themePref === "light" ? SunIcon : themePref === "dark" ? MoonIcon : MonitorIcon
  const label = `${t("core:sidebar.appearance")} · ${t(`core:theme.${themePref}`)}`
  return <Tooltip><TooltipTrigger render={<button type="button" aria-label={label} data-theme-cycle={themePref} onClick={() => setTheme(order[(order.indexOf(themePref) + 1) % order.length])} />}
    className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors duration-120 ease-(--ease-out) hover:bg-sidebar-accent hover:text-foreground", className)}>
    <I className="size-4" strokeWidth={1.75} /></TooltipTrigger><TooltipContent side="top">{label}</TooltipContent></Tooltip>
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
const PILL: Record<string, string> = { run: "bg-blue/10 text-blue", wait: "bg-attention/12 text-attention", fail: "bg-destructive/10 text-destructive", done: "bg-success/10 text-success" }
/** `pill`: a tinted capsule, for the one status a page is about */
export function Status({ s, sess = false, pill = false, className }: { s: string; sess?: boolean; pill?: boolean; className?: string }) {
  const label = (sess ? SS : ST)[s]?.[0] ?? ""
  return <span className={cn("inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap type-footnote text-muted-foreground", pill && cn("h-6 rounded-full px-2.5 font-medium", PILL[s] ?? "bg-muted"), className)} data-status={s}><StatusDot s={s} />{label}</span>
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
/** a "·" between parts, as its own element after each part but the last. The part keeps a dot-wide right margin and
 *  the dot, pulled back over it, takes no width of its own: it stays on the line of the part before it (a wrapped line
 *  never starts with one), and sits outside the part, so a part that truncates, has a border or a gap keeps it whole.
 *  Only after a part wider than the whole line can a dot open the next line; it then lies left of the box, clipped.
 *  Dots around a part that renders nothing collapse. */
export const META_SEP = "overflow-x-clip [&>:not([data-meta-sep]):not(:last-child)]:mr-4 [&>[data-meta-sep]:first-child]:hidden [&>[data-meta-sep]:last-child]:hidden [&>[data-meta-sep]+[data-meta-sep]]:hidden"
export const metaParts = (children: ReactNode) => Children.toArray(children).flatMap((c, n) => n ? [<span key={"sep" + n} data-meta-sep aria-hidden className="-ml-4 w-4 shrink-0 text-center text-tertiary">·</span>, c] : [c])
export const Meta = ({ children, className }: { children: ReactNode; className?: string }) => (
  <div className={cn("flex min-w-0 flex-wrap items-center gap-y-1 type-footnote text-muted-foreground", META_SEP, className)}>{metaParts(children)}</div>
)
export function SectionLabel({ children, right, className }: { children: ReactNode; right?: ReactNode; className?: string }) {
  return <div className={cn("mb-2 flex min-h-6 items-center justify-between gap-2 type-label text-muted-foreground", className)}><span>{children}</span>{right}</div>
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
/** a tool permission question as the Hub words it (“允许执行：<command>”), led in the UI language */
export const permissionQuestion = (q: string) => (lang() === "zh-CN" ? q : q.replace(/^允许执行(?:[：:]\s*(?=\S))?/, (m) => t("core:wait.allowRun") + (m.length > 4 ? ": " : "")))
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
/** view switcher (shadcn Tabs) */
export function Segmented<T extends string>({ value, onChange, options, className, iconOnly = false, size = "default", label }: { value: T; onChange: (v: T) => void; options: readonly (T | readonly [T, ReactNode, string?])[]; className?: string; iconOnly?: boolean; size?: "default" | "sm"; label?: string }) {
  return (
    <Tabs value={value} onValueChange={(v) => onChange(v as T)} className={className}>
      <TabsList aria-label={label} className={cn("bg-muted", size === "sm" && "h-7!")}>
        {options.map((o) => {
          const [v, l, tip] = typeof o === "string" ? [o, o, undefined] : o
          return <TabsTrigger key={v} value={v} aria-label={tip} title={tip} data-seg={v} className={cn("px-3 type-callout font-medium", iconOnly && "px-2", size === "sm" && "px-2.5 type-footnote font-medium")}>{l}</TabsTrigger>
        })}
      </TabsList>
    </Tabs>
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
/** `action`: what to do about it, inside the empty state rather than a page-height below it */
export function Empty({ title, sub, icon, action }: { title: string; sub?: string; icon?: ReactNode; action?: ReactNode }) {
  return (
    <UIEmpty className="px-4 py-16 anim-in sm:px-12">
      <EmptyHeader>
        {icon && <EmptyMedia variant="icon">{icon}</EmptyMedia>}
        <EmptyTitle className="type-headline">{title}</EmptyTitle>
        {sub && <EmptyDescription className="type-callout text-muted-foreground">{sub}</EmptyDescription>}
      </EmptyHeader>
      {action && <EmptyContent>{action}</EmptyContent>}
    </UIEmpty>
  )
}

/* ───────── page frame: large title that collapses into the bar on scroll ───────── */
export function Page({ title, sub, back, actions, toolbar, toolbarTop = false, children, footer, max = 760, bar, noTitle = false, scrollRef, small = false }: {
  /** pin the toolbar to the title line instead of the bottom of the title block */
  toolbarTop?: boolean
  small?: boolean; title: ReactNode; sub?: ReactNode; back?: [string, string]; actions?: ReactNode; toolbar?: ReactNode; children: ReactNode; footer?: ReactNode; max?: number; bar?: ReactNode; noTitle?: boolean; scrollRef?: React.RefObject<HTMLDivElement | null>
}) {
  const [scrolled, setScrolled] = useState(false)
  const own = useRef<HTMLDivElement>(null)
  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className={cn("relative z-10 flex h-12 shrink-0 items-center gap-2 px-3 transition-[border-color,background-color] duration-180 sm:px-5", scrolled || noTitle ? "border-b bg-background/80 backdrop-blur-xl" : "border-b border-transparent")}>
        <SidebarTrigger className="-ml-1 hidden text-muted-foreground md:inline-flex" />
        {back ? (
          /* beside a custom bar the way back keeps its width and the bar truncates; it used to slide under the bar */
          <Breadcrumb className={bar ? "shrink-0" : "min-w-0"}>
            <BreadcrumbList className="flex-nowrap">
              <BreadcrumbItem className="shrink-0"><BreadcrumbLink href={back[0]} className="inline-flex items-center gap-0.5 whitespace-nowrap" data-back><ChevronLeftIcon className="size-4 md:hidden" />{back[1]}</BreadcrumbLink></BreadcrumbItem>
              {!bar && <><BreadcrumbSeparator className={cn("transition-opacity", scrolled || noTitle ? "opacity-100" : "opacity-0")} />
                <BreadcrumbItem className="min-w-0"><BreadcrumbPage className={cn("truncate font-medium transition-opacity", scrolled || noTitle ? "opacity-100" : "opacity-0")}>{title}</BreadcrumbPage></BreadcrumbItem></>}
            </BreadcrumbList>
          </Breadcrumb>
        ) : null}
        {bar ?? (!back && <span className={cn("min-w-0 truncate type-headline transition-opacity duration-180", scrolled || noTitle ? "opacity-100" : "opacity-0")}>{title}</span>)}
        <span className="flex-1" />
        <div className="flex shrink-0 items-center gap-1.5">{actions}</div>
      </header>
      <div ref={scrollRef ?? own} onScroll={(e) => setScrolled(e.currentTarget.scrollTop > 36)} className="min-h-0 flex-1 overflow-y-auto">
        {/* one frame for every page (1040, or 1280 for wide ones) so a title is always in the same place; a reading
          * page keeps its narrower column on the frame's left edge instead of centring a width of its own */}
        <div className={cn("mx-auto w-full px-4 sm:px-8", footer ? "pb-6" : "pb-24 md:pb-12")} style={{ maxWidth: frameOf(max) + 64 }}>
          <div style={{ maxWidth: max <= 820 ? READING : undefined }}>
          {/* the title block keeps 20rem before the toolbar beside it wraps below, so a phone never squeezes the description */}
          {!noTitle && (
            <div className="flex flex-wrap items-end gap-x-4 gap-y-3 pb-8 pt-3">
              <div className="min-w-[min(100%,20rem)] flex-1"><h1 className={cn(small ? "type-title" : "type-display", "text-foreground")}>{title}</h1>{sub && <div className="mt-1.5 type-callout text-muted-foreground">{sub}</div>}</div>
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
/** the page widths: reading (conversation-like and settings pages), standard, wide (stats, an item with its side column) */
const READING = 760
const frameOf = (max: number) => (max > 1080 ? 1280 : 1040)

/* ───────── shell ───────── */
type NavGroup = "work" | "connect" | "automate"
const NAV_GROUPS: NavGroup[] = ["work", "connect", "automate"]
const NAV: { h: string; l: "chat" | "inbox" | "items" | "projects" | "sessions" | "stats" | "integrations" | "channels" | "mail" | "contacts" | "knowledge" | "bots" | "auto" | "signals"; I: any; group: NavGroup }[] = [
  { h: "", l: "chat", I: MessageCircleIcon, group: "work" },
  { h: "inbox", l: "inbox", I: InboxIcon, group: "work" },
  { h: "items", l: "items", I: ListTodoIcon, group: "work" },
  { h: "projects", l: "projects", I: FolderIcon, group: "work" },
  { h: "sessions", l: "sessions", I: SquareTerminalIcon, group: "work" },
  { h: "stats", l: "stats", I: ChartNoAxesColumnIcon, group: "work" },
  { h: "integrations", l: "integrations", I: PlugIcon, group: "connect" },
  { h: "channels", l: "channels", I: MessagesSquareIcon, group: "connect" },
  { h: "mail", l: "mail", I: MailIcon, group: "connect" },
  { h: "contacts", l: "contacts", I: ContactRoundIcon, group: "connect" },
  { h: "knowledge", l: "knowledge", I: BookOpenIcon, group: "connect" },
  { h: "bots", l: "bots", I: BotIcon, group: "automate" },
  { h: "auto", l: "auto", I: WorkflowIcon, group: "automate" },
  { h: "signals", l: "signals", I: RadioIcon, group: "automate" },
]
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
export function Sidebar({ route: full }: { route: string }) {
  const t = useT()
  const route = full.split("?")[0] // #/knowledge?mem=… still highlights 知识
  const S = st.bots.steve ?? Object.values(st.bots)[0], n = pendingCards().length
  const people = usePeopleOn(), meOn = route === "contacts" && location.hash.startsWith("#/contacts/me")
  const cnt: Record<string, number> = { inbox: n, items: st.items.filter((i) => itemDisplayStatus(i) === "run" && !i.arch).length }
  const [nav, more] = useScrollMore<HTMLDivElement>("y")
  /* the page you are on stays in view when the groups scroll */
  useEffect(() => {
    const el = nav.current, on = el?.querySelector('[aria-current="page"]')
    if (!el || !on) return
    const b = el.getBoundingClientRect(), r = on.getBoundingClientRect(), fade = 32
    if (r.top < b.top + fade || r.bottom > b.bottom - fade) el.scrollTop += r.top - b.top - (b.height - r.height) / 2
  }, [route])
  const item = (h: string, l: string, I: any, on: boolean) => (
    <SidebarMenuItem key={h}>
      <SidebarMenuButton isActive={on} tooltip={l} render={<a href={"#/" + h} data-nav={h || "chat"} aria-current={on ? "page" : undefined} />} className="type-callout text-foreground/75 hover:text-foreground data-active:font-medium data-active:text-foreground data-active:[&>svg]:text-primary">
        <I className="size-4! text-muted-foreground transition-colors" strokeWidth={1.75} /><span>{l}</span>
      </SidebarMenuButton>
      {!!cnt[h] && (h === "inbox"
        ? <SidebarMenuBadge className="h-4.5 min-w-4.5 rounded-full bg-attention px-1.5 type-label font-semibold tabular-nums text-white! peer-hover/menu-button:text-white!">{cnt[h]}</SidebarMenuBadge>
        : <SidebarMenuBadge className="type-footnote tabular-nums text-muted-foreground">{cnt[h]}</SidebarMenuBadge>)}
    </SidebarMenuItem>
  )
  const foot = (h: string, l: string, I: any, on: boolean) => (
    <Tooltip key={h}><TooltipTrigger render={<a href={"#/" + h} data-nav={h} aria-current={on ? "page" : undefined} aria-label={l} />}
      className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors duration-120 ease-(--ease-out) hover:bg-sidebar-accent hover:text-foreground", on && "bg-sidebar-accent text-primary hover:text-primary")}>
      <I className="size-4" strokeWidth={1.75} /></TooltipTrigger><TooltipContent side="top">{l}</TooltipContent></Tooltip>
  )
  return (
    <UISidebar collapsible="icon" className="border-r border-sidebar-border [&_[data-slot=sidebar-inner]]:backdrop-blur-2xl">
      <SidebarHeader className="pt-3">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton size="lg" tooltip={S?.n} render={<a href="#/bots/steve" />}>
              <BotAvatar k="steve" size={28} className="size-7!" />
              <span className="min-w-0"><span className="block truncate type-headline">{S?.n}</span><span className="block type-footnote text-muted-foreground">{t("core:sidebar.assistant")}</span></span>
            </SidebarMenuButton>
          </SidebarMenuItem>
          <WorkspaceSwitcher /><HubSwitcher />
          {st.yolo && <SidebarMenuItem className="group-data-[collapsible=icon]:hidden" data-sidebar-yolo><YoloPill className="h-8 w-full rounded-lg px-2.5" /></SidebarMenuItem>}
          <SidebarMenuItem>
            <SidebarMenuButton tooltip={t("core:sidebar.searchTip")} onClick={() => openSheet("command")} className="text-muted-foreground" data-open-command>
              <SearchIcon className="size-4!" strokeWidth={1.5} /><span className="flex-1">{t("common:action.search")}</span><Kbd className="group-data-[collapsible=icon]:hidden">⌘K</Kbd>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      {/* a short window gives the groups less room than they need: they scroll between header and footer. The edge
       *  with more beyond it fades and the footer keeps a hairline, so the lower groups read as scrolled, not cut off */}
      <SidebarContent ref={nav} className="[scrollbar-width:thin]! data-[more~=end]:mask-b-from-[calc(100%-2rem)] data-[more~=start]:mask-t-from-[calc(100%-2rem)]" data-sidebar-nav data-more={more}>
        {NAV_GROUPS.map((group) => <SidebarGroup key={group} className="py-1">
          <SidebarGroupLabel className="h-7 type-label text-muted-foreground">{t(`core:navGroup.${group}`)}</SidebarGroupLabel>
          <SidebarGroupContent><SidebarMenu>{NAV.filter((x) => x.group === group).map((x) => item(x.h, t(`core:nav.${x.l}`), x.I, (route === x.h && !(x.h === "contacts" && meOn)) || (x.h === "contacts" && (route === "orgs" || route === "teams"))))}</SidebarMenu></SidebarGroupContent>
        </SidebarGroup>)}
      </SidebarContent>
      {/* the footer is one row of places (me, devices, accounts, settings) beside the theme, and the versions under it:
       *  short, so the nav groups above keep their room on a laptop screen */}
      <SidebarFooter className="gap-1 border-t border-sidebar-border">
        <div className="flex items-center gap-0.5 group-data-[collapsible=icon]:flex-col">
          {people && foot("contacts/me", t("core:nav.me"), UserRoundIcon, meOn)}
          {foot("devices", t("core:nav.devices"), ServerIcon, route === "devices")}
          {foot("accounts", t("core:nav.accounts"), KeyRoundIcon, route === "accounts" || route === "vault")}
          {foot("settings", t("core:nav.settings"), SettingsIcon, route === "settings" || route === "plugins")}
          <span className="flex-1 group-data-[collapsible=icon]:hidden" />
          <ThemeCycle className="group-data-[collapsible=icon]:hidden" />
        </div>
        {/* ST-21: this page's frontend and the Hub's backend; the full numbers are in 设置 · 关于 */}
        <a href="#/settings/about" title={t("core:sidebar.versions", { web: LOADED_WEB_VERSION, hub: st.version || t("pages:version.fetching") })} className="block truncate rounded-lg px-2 py-0.5 type-footnote tabular-nums text-muted-foreground hover:bg-sidebar-accent hover:text-foreground group-data-[collapsible=icon]:hidden" data-sidebar-versions>{t("core:sidebar.versions", { web: shortVersion(LOADED_WEB_VERSION), hub: st.version ? shortVersion(st.version) : t("core:sidebar.versionUnknown") })}</a>
        <HubUpdateNotice />
        {!!st.webVersion && st.webVersion !== LOADED_WEB_VERSION && <a href="#/settings/about" className="block rounded-lg px-2 py-1 type-footnote text-primary hover:bg-muted group-data-[collapsible=icon]:hidden" data-sidebar-web-update>{t("core:sidebar.webUpdate")}</a>}
      </SidebarFooter>
      <SidebarRail />
    </UISidebar>
  )
}
/** A newer Steve release the Hub found on its own: leads to Settings · 关于, where it is installed. */
export function HubUpdateNotice() {
  const t = useT()
  if (!st.update) return null
  return <a href="#/settings/about" className="flex items-center gap-2 rounded-lg px-2 py-1 type-footnote text-primary hover:bg-muted group-data-[collapsible=icon]:hidden" data-sidebar-hub-update={st.update.version}>
    <span className="size-1.5 shrink-0 rounded-full bg-attention" aria-hidden />{t("core:sidebar.hubUpdate", { version: st.update.version })}
  </a>
}
export function MobileTabs({ route: full }: { route: string }) {
  const tr = useT()
  const route = full.split("?")[0]
  const n = pendingCards().length
  const tabs = [{ h: "", l: tr("core:nav.chat"), I: MessageCircleIcon }, { h: "inbox", l: tr("core:nav.inbox"), I: InboxIcon, b: n }, { h: "items", l: tr("core:nav.items"), I: ListTodoIcon }, { h: "more", l: tr("core:nav.mine"), I: UserIcon, dot: !!st.update }]
  const more = ["more", "projects", "sessions", "stats", "mail", "signals", "settings", "bots", "design", "auto", "plugins", "personas", "contacts", "orgs", "teams", "accounts", "vault", "devices", "knowledge", "github", "yolo"]
  return (
    <nav className="fixed inset-x-0 bottom-0 z-40 flex h-15 bg-background/95 pb-[env(safe-area-inset-bottom)] shadow-[0_-1px_0_var(--border)] backdrop-blur-2xl backdrop-saturate-180 md:hidden">
      {tabs.map((t) => {
        const on = route === t.h || (t.h === "more" && more.includes(route))
        return (
          <a key={t.h} href={"#/" + t.h} data-tab={t.h || "chat"} className={cn("relative flex flex-1 flex-col items-center justify-center gap-0.5 type-label transition-[color,transform] duration-120 active:scale-95", on ? "text-primary" : "text-muted-foreground")}>
            <t.I className="size-5.5" strokeWidth={on ? 1.9 : 1.5} />{t.l}
            {t.b ? <span className="absolute left-1/2 top-1.5 ml-2 flex h-4 min-w-4 items-center justify-center rounded-full bg-attention px-1 type-label leading-none font-semibold tabular-nums text-white ring-2 ring-background">{t.b}</span> : null}
            {"dot" in t && t.dot ? <span className="absolute left-1/2 top-2 ml-2.5 size-2 rounded-full bg-attention ring-2 ring-background" data-tab-update-dot /> : null}
          </a>
        )
      })}
    </nav>
  )
}

/* ───────── modal state ───────── */
type SheetState = { kind: string; payload?: any } | null
let sheet: SheetState = null
export const openSheet = (kind: string, payload?: any) => { sheet = { kind, payload }; emit() }
export const closeSheet = () => { sheet = null; emit() }
export const getSheet = () => sheet

/* ───────── composer (InputGroup) ───────── */
/** the composer's slash commands: [the token the Hub reads after "/", the command id]; label and description by id */
export const COMMANDS: [string, "newItem" | "newProject" | "digest" | "newBot"][] = [["新事项", "newItem"], ["新项目", "newProject"], ["汇报", "digest"], ["新 Bot", "newBot"]]
type MenuKind = "#" | "@" | "/"
/* what the composer marks in its text: the same tokens the Hub reads (an item id, “@名字”, a leading “/命令”), plus the one being typed */
type Mark = "item" | "bot" | "party" | "cmd" | "typing"
const MARK: Record<Mark, string> = {
  item: "bg-blue/12 text-blue ring-blue/12", bot: "bg-blue/12 text-blue ring-blue/12",
  party: "bg-foreground/10 text-foreground ring-foreground/10", cmd: "bg-foreground/10 text-foreground ring-foreground/10",
  typing: "bg-foreground/[0.05] text-muted-foreground ring-foreground/[0.05]",
}
const TOKEN = /(?<![\w@#-])#?[A-Za-z]{1,6}-\d+(?![\w-])|(?<!\S)@[^\s#@]+|^\/\S+/g
export function markText(v: string, at: (ins: string) => "bot" | "party" | undefined, typing?: [number, number] | null): [string, Mark | null][] {
  const rs: [number, number, Mark][] = []
  for (const m of v.matchAll(TOKEN)) {
    const t = m[0], k: Mark | undefined = t[0] === "@" ? at(t) : t[0] === "/" ? (COMMANDS.some(([n]) => "/" + n.replace(/\s+/g, "") === t.replace(/\s+/g, "")) ? "cmd" : undefined)
      : st.items.some((i) => i.id === t.replace(/^#/, "").toUpperCase()) ? "item" : undefined
    if (k) rs.push([m.index, m.index + t.length, k])
  }
  if (typing && typing[1] > typing[0] && !rs.some(([a, b]) => a < typing[1] && typing[0] < b)) rs.push([typing[0], typing[1], "typing"])
  rs.sort((a, b) => a[0] - b[0])
  const out: [string, Mark | null][] = []
  let at0 = 0
  for (const [a, b, k] of rs) { if (a > at0) out.push([v.slice(at0, a), null]); out.push([v.slice(a, b), k]); at0 = b }
  if (at0 < v.length) out.push([v.slice(at0), null])
  return out
}
/** what a page may do with its composer: a withdrawn queued message goes back into it when it is empty */
export type ComposerHandle = { empty: () => boolean; fill: (text: string, files: RFile[]) => void; focus: () => void }
type ComposerProps = {
  handle?: { current: ComposerHandle | null }
  draftKey?: string
  /** `clientId`: the same for every retry of one unchanged draft (send idempotency) */
  placeholder: string; onSend: (t: string, files: string[], refs: RFile[], mentions: { kind: "party" | "bot"; id: string }[], clientId: string) => void | boolean | Promise<void | boolean>; model?: string; onModel?: (m: string) => void; left?: ReactNode; autoFocus?: boolean; className?: string; testId?: string; disabled?: boolean
  /** `busy`: the other side is working. With `onStop` there is a stop button beside send (and Esc), and sending still
   * works: the page queues what is sent meanwhile */
  busy?: boolean; onStop?: () => void | Promise<void>; onCommand?: (c: string) => void
}
export function Composer(props: ComposerProps) {
  return <ComposerInput key={props.draftKey} {...props} />
}
function ComposerInput({ placeholder, onSend, model, left, autoFocus, className, testId, disabled, busy, onStop, onCommand, handle, draftKey }: ComposerProps) {
  const tr = useT()
  const [initial] = useState(() => readDraft(draftKey))
  const [v, setV] = useState(initial.text)
  const [sending, setSending] = useState(false)
  /* stop was pressed and the turn has not ended yet; a stop that never lands frees the button again */
  const [stopping, setStopping] = useState(false)
  useEffect(() => { if (!busy) setStopping(false) }, [busy])
  useEffect(() => { if (!stopping) return; const t = setTimeout(() => setStopping(false), 10_000); return () => clearTimeout(t) }, [stopping])
  const stop = () => { if (!onStop || stopping) return; setStopping(true); void Promise.resolve(onStop()).catch(() => setStopping(false)) }
  const queues = !!busy && !!onStop
  const inFlight = useRef(false)
  const up = useUploads(initial.files, draftKey, (files) => {
    const draft = readDraft(draftKey)
    saveDraft(draftKey, { ...draft, files })
  })
  const [drag, setDrag] = useState(0)
  const [menu, setMenu] = useState<{ kind: MenuKind; q: string; start: number; end: number } | null>(null)
  const [sel, setSel] = useState(0)
  const ta = useRef<HTMLTextAreaElement>(null)
  const chat = testId === "chat"
  const men = useMentions(chat && menu?.kind === "@" ? menu.q : null, initial.mentions)
  useEffect(() => { saveDraft(draftKey, { text: v, files: up.ready, mentions: men.snapshot() }) }, [draftKey, v, up.list])
  /* attachments go by id once uploaded; sending waits until every upload has finished (failed ones are just left out) */
  const live = useRef({ v, n: up.list.length }); live.current = { v, n: up.list.length }
  useEffect(() => {
    if (!handle) return
    handle.current = { empty: () => !live.current.v.trim() && !live.current.n, focus: () => ta.current?.focus(), fill: (t, fs) => { setV(t); up.put(fs); requestAnimationFrame(() => { ta.current?.focus(); ta.current?.setSelectionRange(t.length, t.length) }) } }
    return () => { handle.current = null }
  }, [handle])
  const send = async () => {
    const t = v.trim()
    if (disabled || (busy && !onStop) || inFlight.current || up.uploading || (!t && !up.ready.length)) return
    const sentDraft = draftToSend(draftKey, { text: v, files: up.ready, mentions: men.snapshot() })
    inFlight.current = true; setSending(true)
    try {
      const sent = await onSend(t || "（附件）", up.ready.map((f) => f.id), up.ready, men.take(t), sentDraft.clientId)
      if (sent === false) return
      if (clearSentDraft(draftKey, sentDraft)) { setV(""); up.clear(); men.clear(); setMenu(null) }
    } catch { /* the action reports its error; keep the draft for retry */ }
    finally { inFlight.current = false; setSending(false) }
  }
  const takeFiles = (fs: FileList | File[] | null | undefined) => { if (!disabled && !inFlight.current && fs && fs.length) { up.add(fs); return true } return false }
  const isFileDrag = (e: React.DragEvent) => [...(e.dataTransfer?.types ?? [])].includes("Files")
  type Opt = { id: string; label: ReactNode; ins: string; cmd?: string }
  const opts: Opt[] = !menu ? [] : menu.kind === "#"
    ? st.items.filter((i) => !i.arch && (!menu.q || i.id.toLowerCase().includes(menu.q.toLowerCase()) || i.t.includes(menu.q))).slice(0, 6).map((i) => ({ id: i.id, ins: i.id, label: <><ItemNo id={i.id} className="w-14" /><span className="min-w-0 flex-1 truncate type-callout">{i.t}</span><Status s={itemDisplayStatus(i)} /></> }))
    : menu.kind === "@" && men.opts ? men.opts
    : menu.kind === "@"
    ? [...Object.keys(st.bots).filter((k) => st.bots[k].on && !st.bots[k].main && (!menu.q || st.bots[k].n.includes(menu.q))).map((k) => ({ id: k, ins: "@" + st.bots[k].n.replace(/\s+/g, ""), label: <><BotAvatar k={k} size={18} /><span className="type-callout">{tr("core:composer.botName", { name: st.bots[k].n })}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{tr("core:composer.handTo", { role: st.bots[k].role })}</span></> })),
      ...st.pp.contacts.filter((c) => !c.me && c.kind !== "未确认" && (!menu.q || [c.n, ...c.aliases].some((x) => x.toLowerCase().includes(menu.q.toLowerCase())))).slice(0, 5).map((c) => ({ id: c.id, ins: "@" + c.n.split(" ")[0], label: <><span className="flex size-4.5 items-center justify-center rounded-full bg-muted type-label leading-none text-muted-foreground">{c.n[0]}</span><span className="type-callout">{c.n}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{tr("core:composer.relate", { info: [st.pp.orgs.find((o) => o.id === c.org)?.n, c.title].filter(Boolean).join(" · ") })}</span></> })),
      ...st.pp.teams.filter((g) => !menu.q || g.n.includes(menu.q)).slice(0, 3).map((g) => ({ id: g.id, ins: "@" + g.n.replace(/\s+/g, ""), label: <><UsersRoundIcon className="size-4.5 text-muted-foreground" /><span className="type-callout">{g.n}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{tr("core:composer.team", { count: g.members.length })}</span></> }))]
    : COMMANDS.filter(([n, c]) => !menu.q || n.includes(menu.q) || tr(`core:cmd.${c}`).toLowerCase().includes(menu.q.toLowerCase())).map(([, c]) => ({ id: c, ins: "", cmd: c, label: <><SlashIcon className="size-3.5 text-muted-foreground" /><span className="type-callout">{tr(`core:cmd.${c}`)}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{tr(`core:cmd.${c}Desc`)}</span></> }))
  const track = (val: string, caret: number) => {
    const before = val.slice(0, caret)
    const m = before.match(/#([^\s#@]{0,24})$/) || (chat ? before.match(/(?:^|\s)@([^\s#@]{0,12})$/) : null) || (chat ? before.match(/^\/(\S{0,12})$/) : null)
    const kind: MenuKind | null = !m ? null : m[0].trim().startsWith("#") ? "#" : m[0].trim().startsWith("@") ? "@" : "/"
    if (kind === "#" && !menu && chat) cue("inspect")
    setMenu(m && kind ? { kind, q: m[1], start: caret - m[0].trimStart().length, end: caret } : null); setSel(0)
  }
  const insert = (o: Opt) => {
    if (!menu) return
    men.pick(o)
    if (o.cmd) { setV(""); setMenu(null); onCommand?.(o.cmd); return }
    const nv = v.slice(0, menu.start) + o.ins + " " + v.slice(menu.end), pos = menu.start + o.ins.length + 1
    setV(nv); setMenu(null); requestAnimationFrame(() => { ta.current?.focus(); ta.current?.setSelectionRange(pos, pos) })
  }
  /* the textarea's own text is transparent; this copy behind it, laid out the same way, shows the marks */
  const mirror = useRef<HTMLDivElement>(null)
  const atKind = (ins: string): "bot" | "party" | undefined => {
    const m = men.known(ins); if (m) return m.kind
    const n = ins.slice(1)
    if (Object.values(st.bots).some((b) => b.on && [b.n, b.n + "Bot"].map((x) => x.replace(/\s+/g, "")).includes(n))) return "bot"
    if (st.pp.contacts.some((c) => !c.me && c.n.split(" ")[0] === n) || st.pp.teams.some((g) => g.n.replace(/\s+/g, "") === n)) return "party"
  }
  const marked = markText(v, atKind, menu && [menu.start, menu.end])
  useLayoutEffect(() => { if (mirror.current && ta.current) mirror.current.scrollTop = ta.current.scrollTop })
  const head = menu?.kind === "#" ? <><HashIcon className="size-3.5" />{tr("core:composer.headItem")}</> : menu?.kind === "@" ? <><AtSignIcon className="size-3.5" />{tr("core:composer.headMention")}</> : <><SlashIcon className="size-3.5" />{tr("core:composer.headCommand")}</>
  return (
    <div className="relative" data-dropzone
      onDragEnter={(e) => { if (isFileDrag(e)) { e.preventDefault(); setDrag((n) => n + 1) } }}
      onDragOver={(e) => { if (isFileDrag(e)) { e.preventDefault(); e.dataTransfer.dropEffect = "copy" } }}
      onDragLeave={(e) => { if (isFileDrag(e)) setDrag((n) => Math.max(0, n - 1)) }}
      onDrop={(e) => { if (isFileDrag(e)) { e.preventDefault(); setDrag(0); takeFiles(e.dataTransfer.files) } }}>
      {menu && opts.length > 0 && (
        <Command shouldFilter={false} value={opts[sel]?.id} className="absolute inset-x-2 bottom-full z-30 mb-2 h-auto max-w-md rounded-xl shadow-lg ring-1 ring-foreground/6 anim-in dark:bg-popover dark:shadow-[0_16px_40px_-8px_rgb(0_0_0/0.6)] dark:ring-white/10" data-ref-menu={menu.kind}>
          <CommandList>
            {(menu.kind === "@" && men.opts ? men.groups(opts) : [{ head, xs: opts }]).filter((g) => g.xs.length).map((g, k) => (
            <CommandGroup key={k} heading={<span className="inline-flex items-center gap-1.5">{g.head}</span>}>
              {g.xs.map((o) => { const n = opts.indexOf(o); return (
                <CommandItem key={o.id} value={o.id} onMouseDown={(e) => { e.preventDefault(); insert(o) }} onSelect={() => insert(o)} onMouseEnter={() => setSel(n)} data-ref-opt={o.id} className="gap-2.5 rounded-lg">{o.label}</CommandItem>
              ) })}
            </CommandGroup>))}
          </CommandList>
        </Command>
      )}
      <InputGroup className={cn("rounded-2xl border-border bg-card shadow-[var(--lift-hover)] transition-[border-color,box-shadow] duration-180 ease-(--ease-out) dark:border-transparent has-[[data-slot=input-group-control]:focus-visible]:border-ring/50 has-[[data-slot=input-group-control]:focus-visible]:ring-3 has-[[data-slot=input-group-control]:focus-visible]:ring-ring/10", disabled && "opacity-60", drag > 0 && "border-blue ring-3 ring-blue/15", className)} data-composer={testId}>
        {up.list.length > 0 && (
          <InputGroupAddon align="block-start" className="px-3 pt-3">
            <PendingList list={up.list} onRemove={(key) => { if (!inFlight.current) up.remove(key) }} />
          </InputGroupAddon>
        )}
        <div className="relative w-full">
        <div ref={mirror} aria-hidden data-composer-marks className="pointer-events-none absolute inset-0 overflow-hidden [scrollbar-gutter:stable] whitespace-pre-wrap break-words px-4 pb-2 pt-3 type-body text-foreground">
          {marked.map(([t, k], n) => k ? <span key={n} data-mark={k} className={cn("rounded-md ring-2", MARK[k])}>{t}</span> : <Fragment key={n}>{t}</Fragment>)}{v.endsWith("\n") && "\u200b"}
        </div>
        <InputGroupTextarea ref={ta} autoFocus={autoFocus} disabled={disabled || sending} value={v} placeholder={drag > 0 ? tr("core:composer.dropFiles") : placeholder} rows={1}
          onPaste={(e) => { if (e.clipboardData?.files?.length && takeFiles(e.clipboardData.files)) e.preventDefault() }}
          onChange={(e) => { if (chat && !v && e.target.value) cue("input"); else if (chat && v && !e.target.value && !st.thinking) cue("idle"); setV(e.target.value); track(e.target.value, e.target.selectionStart ?? e.target.value.length) }}
          onScroll={(e) => { if (mirror.current) mirror.current.scrollTop = e.currentTarget.scrollTop }}
          className="relative max-h-40 min-h-11 [scrollbar-gutter:stable] px-4 pt-3 focus-visible:bg-transparent dark:focus-visible:bg-transparent type-body leading-5.5 text-transparent caret-foreground placeholder:text-tertiary selection:bg-blue/25 selection:text-transparent"
          onKeyDown={(e) => {
            if (menu && opts.length) {
              if (e.key === "ArrowDown") { e.preventDefault(); setSel((x) => (x + 1) % opts.length); return }
              if (e.key === "ArrowUp") { e.preventDefault(); setSel((x) => (x - 1 + opts.length) % opts.length); return }
              if (e.key === "Enter" || e.key === "Tab") { e.preventDefault(); insert(opts[sel]); return }
              if (e.key === "Escape") { setMenu(null); return }
            }
            if (e.key === "Escape" && queues && !e.nativeEvent.isComposing) { e.preventDefault(); stop(); return }
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); if (!busy || onStop) send() }
          }} />
        </div>
        <InputGroupAddon align="block-end" className="gap-1 px-2.5 pb-2.5">
          <AttachButton onFiles={(f) => takeFiles(f)} disabled={disabled || sending} />
          {model && <span className="px-1.5 type-footnote text-muted-foreground" data-model>{model}</span>}
          {/* a phone gives the caller's toggles the small size, so the composer stays one compact row */}
          {left && <span className="contents max-sm:[&_[data-slot=tabs-list]]:h-7! max-sm:[&_[data-slot=tabs-trigger]]:px-2 max-sm:[&_[data-slot=tabs-trigger]]:type-footnote">{left}</span>}
          <span className="flex-1" />
          <span className={cn("mr-1 hidden items-center gap-1 whitespace-nowrap type-footnote text-tertiary", chat ? "lg:inline-flex" : "xl:inline-flex")}>{chat && <><Kbd>#</Kbd>{tr("core:composer.hintItem")}<Kbd>@</Kbd>Bot<Kbd>/</Kbd>{tr("core:composer.hintCommand")}</>}{!chat && <><Kbd>⏎</Kbd>{tr("core:composer.hintSend")}<Kbd>⇧⏎</Kbd>{tr("core:composer.hintNewline")}</>}</span>
          {queues && <Tooltip><TooltipTrigger render={<InputGroupButton size="icon-sm" variant="outline" aria-label={tr(stopping ? "core:composer.stopping" : "core:composer.stop")} data-stop data-stopping={stopping || undefined} onClick={stop} disabled={stopping} className="rounded-full" />}>
            {stopping ? <Spinner className="size-4" /> : <SquareIcon className="size-3 fill-current" />}</TooltipTrigger><TooltipContent>{stopping ? tr("core:composer.stoppingTip") : <>{tr("core:composer.stopTip")} <Kbd>Esc</Kbd></>}</TooltipContent></Tooltip>}
          <InputGroupButton size="icon-sm" variant="default" aria-label={tr(sending ? "core:composer.submitting" : queues ? "core:composer.queueSend" : "core:composer.send")} title={queues ? tr("core:composer.queueSendTip") : undefined} data-send data-send-queues={queues || undefined} onClick={() => void send()} disabled={sending || up.uploading || (!v.trim() && !up.ready.length) || disabled} className="rounded-full shadow-xs disabled:bg-muted disabled:text-tertiary disabled:opacity-100 disabled:shadow-none">{sending ? <Spinner className="size-4" /> : <ArrowUpIcon className="size-4" strokeWidth={2.25} />}</InputGroupButton>
        </InputGroupAddon>
      </InputGroup>
    </div>
  )
}

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
    <CodeBlock size="sm" className="mt-1 max-w-160 rounded-lg bg-muted/60 ring-0">
      <CodeBlockContent className="max-h-80 overflow-auto type-code">
        {lines.map((l, n) => <CodeBlockLine key={n} variant={l.startsWith("+") ? "added" : l.startsWith("-") ? "removed" : "default"} className={cn(l.startsWith("+") && "bg-success/10 after:text-success", l.startsWith("✓") && "text-success")}>{l.replace(/^[+-] /, "")}</CodeBlockLine>)}
      </CodeBlockContent>
    </CodeBlock>
  )
}
/** a command and what it printed, as a terminal */
function Terminal({ cmd, out, failed }: { cmd: string; out: string; failed: boolean }) {
  return (
    <div className={cn("mt-1 mb-1.5 max-w-160 overflow-hidden rounded-lg bg-code type-code text-foreground shadow-[var(--lift)]", failed && "bg-destructive/6")} data-terminal>
      <div className="whitespace-pre-wrap break-all px-3.5 pt-2.5 pb-2"><span className={cn("select-none", failed ? "text-destructive" : "text-tertiary")}>$ </span>{cmd}</div>
      {out && <pre className={cn("max-h-72 overflow-auto whitespace-pre-wrap break-all px-3.5 pb-2.5 text-muted-foreground", failed && "text-destructive/90")}>{out}</pre>}
    </div>
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
    const body = plan ? (j === lastPlan ? undefined : <SubSteps steps={plan} live={false} />) : v.I === TerminalIcon ? <Terminal cmd={v.full ?? v.subj} out={String(e[4] ?? "").trim()} failed={state === "fail"} /> : e[4] ? <Detail text={e[4]} /> : undefined
    return { t: "tool", key, state, verb, subj: DELEGATE.includes(tool) ? e[10] ?? v.subj : v.subj, mono: v.mono, full: v.full, group, merged, chip, extra: diff, body,
      below: plan && j === lastPlan ? <SubSteps steps={plan} live={live} /> : undefined, attrs: { kind: e[1] } }
  })
}
/** what the agent is doing right now, in a few words */
function Doing({ evs }: { evs: any[] }) {
  const t = useT()
  const run = evs.findLast((e) => e[0] === "tool" && e[5] === 1)
  if (run) { const v = stepView(run); return <><span className="shrink-0">{v.verb}</span>{v.subj && <span className={cn("min-w-0 truncate text-muted-foreground", v.mono && "font-mono type-footnote")}>{v.subj}</span>}</> }
  const last = evs[evs.length - 1]
  return <span>{t(!last ? "core:transcript.preparing" : last[0] === "th" ? "core:transcript.thinking" : last[0] === "a" ? "core:transcript.replying" : last[0] === "ask" ? "core:transcript.awaitingApproval" : "core:transcript.processing")}</span>
}
/* a folded block stays as the reader left it, across re-renders and remounts */
const workOpen = new Map<string, boolean>()
function WorkBlock({ k, g, live }: { k: string; g: Seg; live: boolean }) {
  const t = useT()
  const key = k + ":" + g.idx
  const [open, setOpenS] = useState(() => workOpen.get(key) ?? live)
  useEffect(() => { if (!workOpen.has(key)) setOpenS(live) }, [live, key])
  const setOpen = (v: boolean) => { workOpen.set(key, v); setOpenS(v) }
  const now = useNow(live)
  const stats = workStats(g.evs)
  const ms = live ? now - (g.t0 ?? now) : (g.t1 ?? 0) - (g.t0 ?? 0)
  const cancelled = g.end === "cancelled"
  const items = trailOf(g.evs, g.at ?? [], g.t1, live)
  return (
    <Collapsible open={open} onOpenChange={setOpen} data-work={live ? "live" : "done"} className="anim-in">
      <CollapsibleTrigger className="group/work flex w-full min-w-0 items-center gap-1.5 rounded-lg py-1 text-left type-callout hover:[&>svg]:text-foreground">
        <ChevronDownIcon className="size-4 shrink-0 -rotate-90 text-muted-foreground transition-transform duration-180 ease-(--ease-out) group-aria-expanded/work:rotate-0" />
        <span className={cn("shrink-0", live ? "text-foreground" : "text-foreground/85")}>{live ? <span className="shimmer">{t("core:transcript.working")}</span> : t(cancelled ? "core:transcript.interrupted" : "core:transcript.handled")}</span>
        {(ms > 0 || live) && <span className="shrink-0 tabular-nums text-muted-foreground" data-work-clock>{live ? clock(ms) : fmtDur(ms)}</span>}
        {stats.length > 0 && <span className="min-w-0 truncate text-muted-foreground">· {stats.join(" · ")}</span>}
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="mt-1 flex flex-col">
          <Trail items={items} live={live} tail={live ? 12 : undefined} end={live || !g.end ? undefined : cancelled ? { s: "cancel", label: t("core:transcript.interrupted") } : { s: "end", label: t("core:trail.finished") }} />
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
  if (g.t === "sys") return <div className="flex items-center gap-3 type-footnote text-muted-foreground"><span className="h-px flex-1 bg-border/70" /><span className="max-w-[80%] text-center">{e[1]}</span><span className="h-px flex-1 bg-border/70" /></div>
  return null
}, (a, b) => a.k === b.k && a.live === b.live && a.g.idx === b.g.idx && a.g.t === b.g.t && a.g.t1 === b.g.t1 && a.g.end === b.g.end && a.g.files?.length === b.g.files?.length && sameEvs(a.g.evs, b.g.evs))
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
      {s.st === "run" && liveIdx < 0 && tailOpen && <div className="flex items-center gap-2 type-callout text-blue"><span className="flex size-4 items-center justify-center"><span className="size-2 rounded-full bg-blue dot-running" /></span><span className="shimmer">{t(last?.t === "a" ? "core:transcript.replying" : "core:transcript.startingShort")}</span></div>}
    </div>
  )
}

/** the session's state at a glance, above the composer: running and doing what, waiting on you, or done and how it went */
export function RunBar({ k, onStop }: { k: string; onStop?: () => void }) {
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
  const what = run ? <Doing evs={turn} /> : tone === "wait" ? t("core:runBar.waitWhat") : stats.join(" · ")
  return (
    <div role="status" aria-live="polite" data-runbar={tone}
      className={cn("mb-2 flex min-h-10 items-center gap-2.5 rounded-xl px-3.5 py-2 type-callout transition-colors duration-180 ease-(--ease-out)",
        tone === "run" ? "bg-blue/8" : tone === "wait" ? "bg-attention/10" : "bg-muted/60")}>
      {run ? <span className="flex size-4 shrink-0 items-center justify-center"><StatusDot s="run" /></span>
        : tone === "wait" ? <StatusDot s="wait" className="mx-1" /> : tone === "done" ? <CircleCheckIcon className="size-4 shrink-0 text-success" strokeWidth={1.75} /> : tone === "stop" ? <CircleSlashIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.75} /> : <StatusDot s="ended" className="mx-1" />}
      <span className={cn("shrink-0 font-medium", tone === "run" ? "text-blue" : tone === "wait" ? "text-attention" : "text-foreground/85")}>{label}</span>
      {/* plain text in its own box: a flex row cuts text off without the ellipsis */}
      {what && <span className={cn("flex min-w-0 items-baseline gap-1.5 truncate", run ? "text-foreground/85" : "text-muted-foreground")} data-doing>{typeof what === "string" ? <span className="min-w-0 truncate">{what}</span> : what}</span>}
      <span className="ml-auto shrink-0 type-footnote tabular-nums text-muted-foreground">{run ? clock(now - t0) : endAt && t0 ? t("core:runBar.took", { dur: fmtDur(endAt - t0) }) : ""}</span>
      {run && onStop && <Button size="xs" variant="outline" className="shrink-0 bg-card" onClick={onStop} data-interrupt><SquareIcon className="fill-current" />{t("core:runBar.interrupt")}</Button>}
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
/** the buttons of a failure card in their order, with their look; the words are FAIL_CHOICE */
const FAIL: Record<string, "ghost" | "outline" | "default"> = { look: "ghost", drop: "outline", tosteve: "outline", delivered: "outline", release_waive: "outline", release_retry: "outline", retry: "default", release_live: "default", continue: "default" }
export function DecisionCard({ i, w = i.wait!, showItem = true }: { i: Item; w?: Item["wait"] & {}; showItem?: boolean }) {
  const t = useT()
  const [rework, setRework] = useState(false), [txt, setTxt] = useState(""), [localBusy, setBusy] = useState(false), [free, setFree] = useState(false)
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
      <CardAction className="flex max-w-full items-center gap-1 type-footnote text-muted-foreground"><span className="truncate">{t("core:card.waited", { bot: botName(w.by === "steve" ? "steve" : w.by), since: w.since })}</span>
        {i.id && <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("core:card.snooze")} className="-mr-1.5 text-muted-foreground" data-snooze={i.id} />}><AlarmClockIcon /></DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-40">
            <DropdownMenuRadioGroup value="" onValueChange={(v) => snooze(i.id, v as (typeof SNOOZE_KEYS)[number])}>{SNOOZE_KEYS.map((x) => <DropdownMenuRadioItem key={x} value={x} data-snooze-opt={x}>{snoozeLabel(x)}</DropdownMenuRadioItem>)}</DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>}
        {/* cards that no longer matter: withdraw this one, or give up on the whole item */}
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("common:action.more")} className="-mr-1.5 text-muted-foreground" data-decision-more={i.id} />}><EllipsisIcon /></DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-52">
            {!(w.subject === "mail_draft" && w.options.includes("confirmed_not_sent")) && <DropdownMenuItem disabled={busy} onClick={() => void dismiss(i.id, w.decision)} data-dismiss={i.id}><BellOffIcon />{t(w.subject === "tool_call" ? "core:card.dismissTool" : "core:card.dismiss")}</DropdownMenuItem>}
            {i.id && <><DropdownMenuSeparator />
            <DropdownMenuItem variant="destructive" disabled={busy} onClick={() => setSt(i.id, "drop")} data-drop-item={i.id}><BanIcon />{t("core:card.dropItem")}</DropdownMenuItem></>}
          </DropdownMenuContent>
        </DropdownMenu>
      </CardAction>
      </div>
      {title && <CardTitle className="col-span-full min-w-0 break-words type-headline">{title}</CardTitle>}
      <YoloBadge decision={w.decision} subject={w.subject} />
      {w.busy && <div role="status" className="col-span-full flex items-center gap-1.5 type-footnote text-muted-foreground" data-agent-busy><StatusDot s="run" className="size-1.5" />{t("core:card.agentBusy")}</div>}
    </CardHeader>
  )
  const foot = (c: ReactNode) => <CardFooter className="flex-wrap justify-end gap-2 bg-muted/50 px-4 py-3! dark:bg-muted/40">{c}</CardFooter>
  const draft = <>{w.subject === "chan_message" && (w.type === "approve" || w.type === "fail") && <OutContext id={w.subjectId} />}
    {w.draft ? <div className="max-h-72 overflow-auto break-words type-body text-muted-foreground"><Markdown text={w.draft} /></div> : null}</>
  let body: ReactNode = null
  if (FLOW_SUBJECTS.includes(w.subject)) body = <FlowDecisionBody w={w} act={(c) => void act(c)} busy={busy} head={head} foot={foot} />
  else if (w.type === "confirm" && w.subject === "mem") body = <>{head(memAsk(w.q))}<MemConfirm q={w.q} draft={w.draft} busy={busy} act={(c, t) => void act(c, t)} /></>
  else if (isPeopleDecision(w)) body = <>{head(w.q)}<PeopleDecisionBody w={w} busy={busy} act={(c) => void act(c)} /></>
  else if (w.subject === "mail_draft") body = <MailDraftCard w={w} act={act} busy={busy} head={head} foot={foot} />
  else if (MODULE_CARDS[w.subject]) body = MODULE_CARDS[w.subject]({ w, act, busy, head, foot })
  else if (w.type === "accept") body = <>
    {head(w.q)}
    <CardContent className="pb-4">{draft}
      {rework && <Textarea autoFocus className="mt-3 min-h-20 rounded-lg" placeholder={t("core:card.reworkPlaceholder", { bot: botName(i.b) })} value={txt} onChange={(e) => setTxt(e.target.value)} data-rework />}</CardContent>
    {foot(rework ? <><Button variant="ghost" onClick={() => setRework(false)}>{t("common:action.cancel")}</Button><Button disabled={busy} onClick={() => act("rework", txt.trim())}>{t("core:card.sendRework")}</Button></>
      : <><Button variant="outline" onClick={() => setRework(true)}>{t("core:card.rework")}</Button>{acceptChoosesRelease(i)
        ? <><Button variant="outline" disabled={busy} onClick={() => act("pass", "no-release")} data-pass-no-release>{t("core:card.passNoRelease")}</Button><Button disabled={busy} onClick={() => act("pass", "release")} data-pass data-pass-release><CheckIcon />{t("core:card.passRelease")}</Button></>
        : <Button disabled={busy} onClick={() => act("pass")} data-pass><CheckIcon />{t("core:card.pass")}</Button>}</>)}
  </>
  else if (w.type === "approve" && w.subject === "tool_call") {
    const opts = w.options.map((o) => { const [id, name, kind] = o.split("|"); return { id, name, kind } })
    const always = opts.find((o) => o.kind === "allow_always"), once = opts.find((o) => o.kind === "allow_once")
    body = <>
      {head(t("core:card.allowTool"))}
      <CardContent className="min-w-0 pb-3"><PermissionDetails title={w.q.replace(/^允许执行[：:]\s*/, "")} input={w.draft} /></CardContent>
      {foot(<><Button variant="ghost" className="mr-auto text-muted-foreground" disabled={busy} onClick={() => act("deny")}>{t("core:card.deny")}</Button>
        {always && <Button variant="outline" disabled={busy} onClick={() => act(always.id)} title={always.name}>{t("core:card.allowAlways")}</Button>}
        {once && <Button disabled={busy} onClick={() => act(once.id)} data-allow>{t("core:card.allowOnce")}</Button>}</>)}
    </>
  }
  else if (w.type === "approve") body = <>
    {head(w.q)}
    <CardContent className="pb-4">{draft}{rework && <Textarea autoFocus className="mt-3 min-h-16 rounded-lg" placeholder={t("core:card.rejectPlaceholder")} value={txt} onChange={(e) => setTxt(e.target.value)} />}</CardContent>
    {foot(rework ? <><Button variant="ghost" onClick={() => setRework(false)}>{t("common:action.cancel")}</Button><Button variant="outline" disabled={busy} onClick={() => act("no", txt.trim())}>{t("core:card.reject")}</Button></>
      : <><Button variant="ghost" className="mr-auto text-muted-foreground" onClick={() => setRework(true)}>{t("core:card.rejectMore")}</Button>{w.subject === "chan_message" && w.options.includes("rewrite") && <Button variant="outline" disabled={busy} onClick={() => act("rewrite")} data-rewrite>{t("core:card.rewriteFirst")}</Button>}<Button disabled={busy} onClick={() => act("yes")} data-approve>{t(w.subject === "release" ? "core:card.approveRelease" : "core:card.approve")}</Button></>)}
  </>
  else if (w.type === "propose") body = <>
    {head(w.q)}
    <CardContent className="pb-4">{draft}</CardContent>
    {foot(<><Button variant="outline" disabled={busy} onClick={() => act("no")}>{t("core:card.notNeeded")}</Button><Button disabled={busy} onClick={() => act("yes")} data-propose-yes>{t("core:card.start")}</Button></>)}
  </>
  else if (w.type === "fail") body = <>
    {head(w.q)}
    <CardContent className="pb-4">{draft}{rework && <Textarea autoFocus className="mt-3 min-h-16 rounded-lg" placeholder={t("core:card.nextPlaceholder")} value={txt} onChange={(e) => setTxt(e.target.value)} />}</CardContent>
    {foot(<>{w.options.filter((o) => FAIL[o]).sort((a, b) => Object.keys(FAIL).indexOf(a) - Object.keys(FAIL).indexOf(b)).map((o) => (
      <Button key={o} variant={FAIL[o]} disabled={busy} className={cn(o === "look" && "mr-auto text-muted-foreground")} onClick={() => (o === "continue" && !rework ? setRework(true) : act(o, o === "continue" ? txt.trim() : undefined))} data-fail={o}>{o === "continue" && rework ? t("core:card.sendContinue") : FAIL_CHOICE[o]}</Button>))}</>)}
  </>
  else if (w.type === "ask" && w.subject === "design_implementation") body = <>
    {head(w.q)}
    <CardContent className="pb-4">{draft}<Textarea className="mt-3 min-h-20 rounded-lg" placeholder={t("core:card.designFeedback")} value={txt} onChange={(e) => setTxt(e.target.value)} data-design-feedback /></CardContent>
    {foot(w.options.map((o, n) => <Button key={o} variant={n === 0 ? "default" : "outline"} disabled={busy || (n === 2 && !txt.trim())} onClick={() => void act(o, txt.trim())} data-design-choice={n}>{t(n === 0 ? "core:card.designImplement" : n === 1 ? "core:card.designKeep" : "core:card.designRevise")}</Button>))}
  </>
  /* a typed answer is not one of the questionnaire's answer controls, so its validation would block the submit; send it directly */
  else if (w.type === "ask") { const typed = free || !w.opts?.length; body = <>
    {head(null)}
    <Questionnaire className="gap-0" onSubmit={(ev) => { ev.preventDefault(); const v = String(new FormData(ev.currentTarget).get("answer") ?? ""); if (v) void act("answer", v) }}>
      <QuestionnaireItem name="answer" required={!typed} className={cn("gap-3 px-(--card-spacing) pb-4", w.busy ? "mt-2" : "-mt-2")}>
        <QuestionnaireTitle className="mb-1! break-words type-headline">{w.q}</QuestionnaireTitle>
        {!free && !!w.opts?.length && <QuestionnaireChoices className="-mx-2 gap-0.5">
          {w.opts.map((o) => <QuestionnaireChoice key={o} value={o} className="min-h-10 rounded-lg border-transparent px-2 py-2.5 hover:bg-accent data-checked:border-transparent data-checked:bg-blue/8">{o}</QuestionnaireChoice>)}
        </QuestionnaireChoices>}
        {typed && <Textarea autoFocus className="min-h-16 rounded-lg" placeholder={t("core:card.answerPlaceholder")} value={txt} onChange={(e) => setTxt(e.target.value)} data-free-answer />}
      </QuestionnaireItem>
      <QuestionnaireActions className="flex justify-end gap-2 bg-muted/50 px-4 py-3 dark:bg-muted/40">{!!w.opts?.length && <Button type="button" variant="ghost" className="mr-auto text-muted-foreground" onClick={() => setFree(!free)}>{t(free ? "core:card.fromOptions" : "core:card.ownAnswer")}</Button>}{typed
        ? <Button type="button" data-answer disabled={busy || !txt.trim()} onClick={() => void act("answer", txt.trim())}>{t("core:card.answer")}</Button>
        : <QuestionnaireSubmit data-answer disabled={busy}>{t("core:card.answer")}</QuestionnaireSubmit>}</QuestionnaireActions>
    </Questionnaire>
  </> }
  return <Card size="sm" className="gap-0 py-0 shadow-[var(--lift)] anim-in" data-decision={w.type} data-decision-wrap={i.id || w.decision} aria-busy={busy}>{busy && <div role="status" className="px-4 py-2 type-footnote text-muted-foreground">{t("core:card.submitting")}</div>}{body}</Card>
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
