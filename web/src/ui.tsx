/* Shared building blocks for the Steve prototype — see DESIGN.md.
 * Quiet chrome: grey scale everywhere, blue = selected/running, orange = needs you. */
import { Children, Fragment, memo, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore, type ReactNode } from "react"
import { Robot } from "@/robot"
import { cue } from "@/bus"
import { clockDur } from "@/i18n/format"
import { LANGS, lang, setLang, t, useT, type Lang } from "@/i18n"
import { cn } from "cn"
import { BookOpenIcon, ChartNoAxesColumnIcon,
  ArrowUpIcon, BrainIcon, CheckIcon, ChevronLeftIcon, FileTextIcon, GitForkIcon, InboxIcon, ListTodoIcon,
  MessageCircleIcon, MoonIcon, PencilIcon, RadioIcon, SearchIcon, SettingsIcon, SquareTerminalIcon,
  SunIcon, TerminalIcon, ArrowUpRightIcon, UserIcon, UserRoundIcon, ContactRoundIcon, KeyRoundIcon, UsersRoundIcon, ServerIcon, CornerDownRightIcon, MonitorIcon, WorkflowIcon, HashIcon, BotIcon, AtSignIcon, SlashIcon, XIcon, SquareIcon, AlarmClockIcon, FolderIcon, EllipsisIcon, BellOffIcon, BanIcon,
  MailIcon,
  CircleCheckIcon, CircleIcon, CircleSlashIcon, CircleXIcon, ChevronRightIcon, FileDiffIcon, FilePlusIcon, GlobeIcon, SparklesIcon, WrenchIcon,
  CircleAlertIcon as RelFailIcon, CircleCheckIcon as RelOkIcon, MegaphoneIcon as RelRunIcon,
  PlugIcon, MessagesSquareIcon,
} from "lucide-react"
import { Button } from "@/components/ui/button"
import { Item as UIItem, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemSeparator, ItemTitle } from "@/components/ui/item"
import { Empty as UIEmpty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "@/components/ui/empty"
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card"
import { Breadcrumb, BreadcrumbItem, BreadcrumbLink, BreadcrumbList, BreadcrumbPage, BreadcrumbSeparator } from "@/components/ui/breadcrumb"
import { Command, CommandGroup, CommandItem, CommandList } from "@/components/ui/command"
import { Bubble, BubbleContent } from "@/components/ui/bubble"
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group"
import { Kbd } from "@/components/ui/kbd"
import { Sidebar as UISidebar, SidebarContent, SidebarFooter, SidebarGroup, SidebarGroupLabel, SidebarGroupContent, SidebarHeader, SidebarMenu, SidebarMenuBadge, SidebarMenuButton, SidebarMenuItem, SidebarRail, SidebarSeparator, SidebarTrigger } from "@/components/ui/sidebar"
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
import { st, emit, briefHead, pendingCards, acceptChoosesRelease, ST, SS, WT, FAIL_CHOICE, SNOOZE_KEYS, snoozeLabel, tagCatLabel, botName, decide, dismiss, setSt, snooze, tag, isKind, kindColor, sortTags, loadEvents, type Item, type Reply, type Rel, type Sess } from "@/store"
import { YoloBadge } from "@/yolo/badges"
import { YoloPill } from "@/yolo/switch"
import { WorkspaceSwitcher } from "@/workspace-switcher"
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
const hashSub = (cb: () => void) => { window.addEventListener("hashchange", cb); return () => window.removeEventListener("hashchange", cb) }
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
      className={cn("flex min-w-0 items-start gap-1.5", box ? "rounded-lg border px-3 py-2 type-callout" : "type-footnote", box && rel.tone === "fail" && "border-destructive/40", className)}>
      <I className={cn("mt-[2px] size-3.5 shrink-0", box && "size-4", color)} strokeWidth={2} />
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
  return <span className={cn("inline-flex shrink-0 items-center gap-1.5 whitespace-nowrap type-footnote text-muted-foreground", pill && cn("rounded-full px-2 py-0.5 font-medium", PILL[s] ?? "bg-muted"), className)} data-status={s}><StatusDot s={s} />{label}</span>
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
/** an item's category: an outline pill with its own muted dot, always first and never removable on its own */
export function KindTag({ id, className }: { id: string; className?: string }) {
  const tr = useT(), t = tag(id)
  if (!t) return null
  return <span title={tr("core:tag.kindTitle", { desc: t.desc })} className={cn("inline-flex h-5 shrink-0 items-center gap-1 rounded-full border border-border bg-background px-1.5 font-mono text-[11px] leading-none text-foreground", className)} data-kind={t.name}>
    <span aria-hidden className="size-1.5 rounded-full" style={{ background: kindColor(t.name) }} />{t.name}</span>
}
/** tags: the category first (see KindTag), then quiet grey pills; the tag category shows on hover */
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
        <span key={t.id} title={tr("core:tag.title", { cat: tagCatLabel(t.cat), desc: t.desc })} className="inline-flex h-5 items-center gap-0.5 rounded-full bg-muted px-2 text-[11.5px] leading-none text-muted-foreground" data-tag={t.name}>
          {t.name}{onRemove && <button type="button" aria-label={tr("core:tag.remove", { name: t.name })} onClick={(e) => { e.stopPropagation(); onRemove(t.id) }} className="-mr-1 ml-0.5 flex size-4 items-center justify-center rounded-full hover:bg-background hover:text-foreground"><XIcon className="size-3" /></button>}
        </span>))}
      {max && rest.length > max && <span className="text-[11.5px] text-muted-foreground">+{rest.length - max}</span>}
    </span>
  )
}
export const ItemNo = ({ id, className }: { id: string; className?: string }) => <span className={cn("shrink-0 font-mono text-[12px] tabular-nums text-muted-foreground", className)} data-no>{id}</span>
export const Meta = ({ children, className }: { children: ReactNode; className?: string }) => (
  <div className={cn("flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-1 type-footnote text-muted-foreground [&>*+*]:before:mr-1.5 [&>*+*]:before:text-tertiary [&>*+*]:before:content-['·']", className)}>{children}</div>
)
export function SectionLabel({ children, right, className }: { children: ReactNode; right?: ReactNode; className?: string }) {
  return <div className={cn("mb-2 flex min-h-6 items-center justify-between gap-2 type-label text-muted-foreground", className)}><span>{children}</span>{right}</div>
}
/** grouped list: shadcn ItemGroup in one hairline container, ItemSeparator between rows */
export function Group({ children, className }: { children: ReactNode; className?: string }) {
  const xs = Children.toArray(children).filter(Boolean)
  return (
    <ItemGroup className={cn("-mx-2 gap-0!", className)}>
      {xs.map((c, n) => <Fragment key={n}>{n > 0 && <ItemSeparator className="mx-2 my-0 w-auto!" />}{c}</Fragment>)}
    </ItemGroup>
  )
}
export function Row({ title, sub, children, onClick, icon, className, ...rest }: { title: ReactNode; sub?: ReactNode; children?: ReactNode; onClick?: () => void; icon?: ReactNode; className?: string; [k: string]: any }) {
  return (
    <UIItem size="sm" role="listitem" className={cn("min-h-12 rounded-lg border-0 px-2 py-2.5", onClick && "cursor-pointer text-left hover:bg-accent", className)}
      render={onClick ? <button type="button" onClick={onClick} /> : undefined} {...rest}>
      {icon && <ItemMedia>{icon}</ItemMedia>}
      <ItemContent className="min-w-0 gap-0.5">
        <ItemTitle className="w-full type-body font-normal">{title}</ItemTitle>
        {sub && <ItemDescription className="type-footnote">{sub}</ItemDescription>}
      </ItemContent>
      {children && <ItemActions className="shrink-0">{children}</ItemActions>}
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
          return <TabsTrigger key={v} value={v} aria-label={tip} title={tip} data-seg={v} className={cn("px-3 text-[13px]", iconOnly && "px-2", size === "sm" && "px-2.5 text-[12px]")}>{l}</TabsTrigger>
        })}
      </TabsList>
    </Tabs>
  )
}
/** filters (shadcn ToggleGroup) — single choice, never empty */
export function Chips<T extends string>({ value, onChange, options, className }: { value: T; onChange: (v: T) => void; options: readonly T[]; className?: string }) {
  return (
    <ToggleGroup value={[value]} onValueChange={(v: any[]) => v.length && onChange(v[v.length - 1] as T)} size="sm" spacing={1} className={className}>
      {options.map((o) => <ToggleGroupItem key={o} value={o} data-seg={o} className="h-7 rounded-full px-3 text-[12.5px] text-muted-foreground hover:text-foreground data-pressed:bg-muted data-pressed:text-foreground">{o}</ToggleGroupItem>)}
    </ToggleGroup>
  )
}
export function Empty({ title, sub, icon }: { title: string; sub?: string; icon?: ReactNode }) {
  return (
    <UIEmpty className="py-16">
      <EmptyHeader>
        {icon && <EmptyMedia variant="icon">{icon}</EmptyMedia>}
        <EmptyTitle className="type-headline">{title}</EmptyTitle>
        {sub && <EmptyDescription className="type-callout">{sub}</EmptyDescription>}
      </EmptyHeader>
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
      <header className={cn("relative z-10 flex h-12 shrink-0 items-center gap-2 px-3 transition-[border-color,background-color] duration-200 sm:px-5", scrolled || noTitle ? "border-b bg-background/80 backdrop-blur-xl" : "border-b border-transparent")}>
        <SidebarTrigger className="-ml-1 hidden text-muted-foreground md:inline-flex" />
        {back ? (
          <Breadcrumb className="min-w-0">
            <BreadcrumbList className="flex-nowrap">
              <BreadcrumbItem className="shrink-0"><BreadcrumbLink href={back[0]} className="inline-flex items-center gap-0.5 whitespace-nowrap" data-back><ChevronLeftIcon className="size-4 md:hidden" />{back[1]}</BreadcrumbLink></BreadcrumbItem>
              {!bar && <><BreadcrumbSeparator className={cn("transition-opacity", scrolled || noTitle ? "opacity-100" : "opacity-0")} />
                <BreadcrumbItem className="min-w-0"><BreadcrumbPage className={cn("truncate font-medium transition-opacity", scrolled || noTitle ? "opacity-100" : "opacity-0")}>{title}</BreadcrumbPage></BreadcrumbItem></>}
            </BreadcrumbList>
          </Breadcrumb>
        ) : null}
        {bar ?? (!back && <span className={cn("min-w-0 truncate type-headline transition-opacity duration-200", scrolled || noTitle ? "opacity-100" : "opacity-0")}>{title}</span>)}
        <span className="flex-1" />
        <div className="flex shrink-0 items-center gap-1.5">{actions}</div>
      </header>
      <div ref={scrollRef ?? own} onScroll={(e) => setScrolled(e.currentTarget.scrollTop > 36)} className="min-h-0 flex-1 overflow-y-auto">
        <div className={cn("mx-auto w-full px-4 sm:px-8", footer ? "pb-6" : "pb-24 md:pb-12")} style={{ maxWidth: max + 64 }}>
          {!noTitle && (
            <div className="flex flex-wrap items-end gap-x-4 gap-y-3 pb-6 pt-1">
              <div className="min-w-0 flex-1"><h1 className={cn(small ? "type-title" : "type-large-title", "text-foreground")}>{title}</h1>{sub && <div className="mt-1 type-callout text-muted-foreground">{sub}</div>}</div>
              {toolbar && <div className={cn("flex flex-wrap items-center gap-2", toolbarTop && "self-start")}>{toolbar}</div>}
            </div>
          )}
          {children}
        </div>
      </div>
      {footer}
    </div>
  )
}

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
export function Sidebar({ route: full }: { route: string }) {
  const t = useT()
  const route = full.split("?")[0] // #/knowledge?mem=… still highlights 知识
  const S = st.bots.steve ?? Object.values(st.bots)[0], n = pendingCards().length
  const people = usePeopleOn(), meOn = route === "contacts" && location.hash.startsWith("#/contacts/me")
  const cnt: Record<string, number> = { inbox: n, items: st.items.filter((i) => i.st === "run" && !i.arch).length }
  const item = (h: string, l: string, I: any, on: boolean) => (
    <SidebarMenuItem key={h}>
      <SidebarMenuButton isActive={on} tooltip={l} render={<a href={"#/" + h} data-nav={h || "chat"} aria-current={on ? "page" : undefined} />} className="text-[14px] text-foreground/85 data-active:font-medium data-active:text-foreground">
        <I className="size-[18px]! text-muted-foreground" strokeWidth={1.5} /><span>{l}</span>
      </SidebarMenuButton>
      {!!cnt[h] && (h === "inbox"
        ? <SidebarMenuBadge className="bg-attention px-1.5 text-[11px] font-semibold text-white! peer-hover/menu-button:text-white!">{cnt[h]}</SidebarMenuBadge>
        : <SidebarMenuBadge className="text-muted-foreground">{cnt[h]}</SidebarMenuBadge>)}
    </SidebarMenuItem>
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
          <WorkspaceSwitcher />
          {st.yolo && <SidebarMenuItem className="px-1.5 py-0.5 group-data-[collapsible=icon]:hidden" data-sidebar-yolo><YoloPill /></SidebarMenuItem>}
          <SidebarMenuItem>
            <SidebarMenuButton tooltip={t("core:sidebar.searchTip")} onClick={() => openSheet("command")} className="text-muted-foreground" data-open-command>
              <SearchIcon className="size-4!" strokeWidth={1.5} /><span className="flex-1">{t("common:action.search")}</span><Kbd className="group-data-[collapsible=icon]:hidden">⌘K</Kbd>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      <SidebarContent>
        {NAV_GROUPS.map((group) => <SidebarGroup key={group} className="py-1">
          <SidebarGroupLabel className="h-6 text-[11px] text-muted-foreground">{t(`core:navGroup.${group}`)}</SidebarGroupLabel>
          <SidebarGroupContent><SidebarMenu>{NAV.filter((x) => x.group === group).map((x) => item(x.h, t(`core:nav.${x.l}`), x.I, (route === x.h && !(x.h === "contacts" && meOn)) || (x.h === "contacts" && (route === "orgs" || route === "teams"))))}</SidebarMenu></SidebarGroupContent>
        </SidebarGroup>)}
      </SidebarContent>
      <SidebarFooter>
        <SidebarMenu>
          {people && item("contacts/me", t("core:nav.me"), UserRoundIcon, meOn)}
          {item("devices", t("core:nav.devices"), ServerIcon, route === "devices")}
          {item("accounts", t("core:nav.accounts"), KeyRoundIcon, route === "accounts" || route === "vault")}
          {item("settings", t("core:nav.settings"), SettingsIcon, route === "settings" || route === "plugins")}
        </SidebarMenu>
        <SidebarSeparator className="mx-0" />
        <div className="flex items-center justify-between px-1 pb-1 group-data-[collapsible=icon]:hidden"><span className="type-footnote text-muted-foreground">{t("core:sidebar.appearance")}</span><ThemeSwitch /></div>
        {/* ST-21: this page's frontend and the Hub's backend; the full numbers are in 设置 · 关于 */}
        <a href="#/settings/about" title={t("core:sidebar.versions", { web: LOADED_WEB_VERSION, hub: st.version || t("pages:version.fetching") })} className="block truncate rounded-lg px-2 py-0.5 type-footnote tabular-nums text-muted-foreground hover:bg-muted group-data-[collapsible=icon]:hidden" data-sidebar-versions>{t("core:sidebar.versions", { web: shortVersion(LOADED_WEB_VERSION), hub: st.version ? shortVersion(st.version) : t("core:sidebar.versionUnknown") })}</a>
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
    <nav className="fixed inset-x-0 bottom-0 z-40 flex h-[60px] border-t bg-background/85 pb-[env(safe-area-inset-bottom)] backdrop-blur-xl md:hidden">
      {tabs.map((t) => {
        const on = route === t.h || (t.h === "more" && more.includes(route))
        return (
          <a key={t.h} href={"#/" + t.h} data-tab={t.h || "chat"} className={cn("relative flex flex-1 flex-col items-center justify-center gap-1 text-[10.5px] font-medium", on ? "text-blue" : "text-muted-foreground")}>
            <t.I className="size-[22px]" strokeWidth={on ? 1.9 : 1.5} />{t.l}
            {t.b ? <span className="absolute left-1/2 top-1.5 ml-2 flex h-4 min-w-4 items-center justify-center rounded-full bg-attention px-1 text-[10px] font-semibold text-white">{t.b}</span> : null}
            {"dot" in t && t.dot ? <span className="absolute left-1/2 top-2 ml-2.5 size-2 rounded-full bg-attention" data-tab-update-dot /> : null}
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
export type ComposerHandle = { empty: () => boolean; fill: (text: string, files: RFile[]) => void }
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
    handle.current = { empty: () => !live.current.v.trim() && !live.current.n, fill: (t, fs) => { setV(t); up.put(fs); requestAnimationFrame(() => { ta.current?.focus(); ta.current?.setSelectionRange(t.length, t.length) }) } }
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
    ? st.items.filter((i) => !i.arch && (!menu.q || i.id.toLowerCase().includes(menu.q.toLowerCase()) || i.t.includes(menu.q))).slice(0, 6).map((i) => ({ id: i.id, ins: i.id, label: <><ItemNo id={i.id} className="w-14" /><span className="min-w-0 flex-1 truncate type-callout">{i.t}</span><Status s={i.st} /></> }))
    : menu.kind === "@" && men.opts ? men.opts
    : menu.kind === "@"
    ? [...Object.keys(st.bots).filter((k) => st.bots[k].on && !st.bots[k].main && (!menu.q || st.bots[k].n.includes(menu.q))).map((k) => ({ id: k, ins: "@" + st.bots[k].n.replace(/\s+/g, ""), label: <><BotAvatar k={k} size={18} /><span className="type-callout">{tr("core:composer.botName", { name: st.bots[k].n })}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{tr("core:composer.handTo", { role: st.bots[k].role })}</span></> })),
      ...st.pp.contacts.filter((c) => !c.me && c.kind !== "未确认" && (!menu.q || [c.n, ...c.aliases].some((x) => x.toLowerCase().includes(menu.q.toLowerCase())))).slice(0, 5).map((c) => ({ id: c.id, ins: "@" + c.n.split(" ")[0], label: <><span className="flex size-[18px] items-center justify-center rounded-full bg-muted text-[10px]">{c.n[0]}</span><span className="type-callout">{c.n}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{tr("core:composer.relate", { info: [st.pp.orgs.find((o) => o.id === c.org)?.n, c.title].filter(Boolean).join(" · ") })}</span></> })),
      ...st.pp.teams.filter((g) => !menu.q || g.n.includes(menu.q)).slice(0, 3).map((g) => ({ id: g.id, ins: "@" + g.n.replace(/\s+/g, ""), label: <><UsersRoundIcon className="size-[18px] text-muted-foreground" /><span className="type-callout">{g.n}</span><span className="min-w-0 flex-1 truncate type-footnote text-muted-foreground">{tr("core:composer.team", { count: g.members.length })}</span></> }))]
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
        <Command shouldFilter={false} value={opts[sel]?.id} className="absolute inset-x-2 bottom-full z-30 mb-2 h-auto max-w-[460px] rounded-xl border shadow-lg anim-in" data-ref-menu={menu.kind}>
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
      <InputGroup className={cn("rounded-[20px] border-border bg-card shadow-[0_2px_12px_rgb(0_0_0/0.05)] dark:shadow-none has-[[data-slot=input-group-control]:focus-visible]:border-foreground/25 has-[[data-slot=input-group-control]:focus-visible]:ring-0", disabled && "opacity-60", drag > 0 && "border-blue ring-2 ring-blue/20", className)} data-composer={testId}>
        {up.list.length > 0 && (
          <InputGroupAddon align="block-start" className="px-3 pt-3">
            <PendingList list={up.list} onRemove={(key) => { if (!inFlight.current) up.remove(key) }} />
          </InputGroupAddon>
        )}
        <div className="relative w-full">
        <div ref={mirror} aria-hidden data-composer-marks className="pointer-events-none absolute inset-0 overflow-hidden [scrollbar-gutter:stable] whitespace-pre-wrap break-words px-4 pb-2 pt-3 text-[14px] leading-[22px] text-foreground">
          {marked.map(([t, k], n) => k ? <span key={n} data-mark={k} className={cn("rounded-[4px] ring-2", MARK[k])}>{t}</span> : <Fragment key={n}>{t}</Fragment>)}{v.endsWith("\n") && "\u200b"}
        </div>
        <InputGroupTextarea ref={ta} autoFocus={autoFocus} disabled={disabled || sending} value={v} placeholder={drag > 0 ? tr("core:composer.dropFiles") : placeholder} rows={1}
          onPaste={(e) => { if (e.clipboardData?.files?.length && takeFiles(e.clipboardData.files)) e.preventDefault() }}
          onChange={(e) => { if (chat && !v && e.target.value) cue("input"); else if (chat && v && !e.target.value && !st.thinking) cue("idle"); setV(e.target.value); track(e.target.value, e.target.selectionStart ?? e.target.value.length) }}
          onScroll={(e) => { if (mirror.current) mirror.current.scrollTop = e.currentTarget.scrollTop }}
          className="relative max-h-40 min-h-11 [scrollbar-gutter:stable] px-4 pt-3 text-[14px] leading-[22px] text-transparent caret-foreground placeholder:text-tertiary selection:bg-blue/25 selection:text-transparent"
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
          {left}
          <span className="flex-1" />
          <span className="mr-1 hidden items-center gap-1 type-footnote text-tertiary lg:inline-flex">{chat && <><Kbd>#</Kbd>{tr("core:composer.hintItem")}<Kbd>@</Kbd>Bot<Kbd>/</Kbd>{tr("core:composer.hintCommand")}</>}{!chat && <><Kbd>⏎</Kbd>{tr("core:composer.hintSend")}<Kbd>⇧⏎</Kbd>{tr("core:composer.hintNewline")}</>}</span>
          {queues && <Tooltip><TooltipTrigger render={<InputGroupButton size="icon-sm" variant="outline" aria-label={tr(stopping ? "core:composer.stopping" : "core:composer.stop")} data-stop data-stopping={stopping || undefined} onClick={stop} disabled={stopping} className="rounded-full" />}>
            {stopping ? <Spinner className="size-4" /> : <SquareIcon className="size-3 fill-current" />}</TooltipTrigger><TooltipContent>{stopping ? tr("core:composer.stoppingTip") : <>{tr("core:composer.stopTip")} <Kbd>Esc</Kbd></>}</TooltipContent></Tooltip>}
          <InputGroupButton size="icon-sm" variant="default" aria-label={tr(sending ? "core:composer.submitting" : queues ? "core:composer.queueSend" : "core:composer.send")} title={queues ? tr("core:composer.queueSendTip") : undefined} data-send data-send-queues={queues || undefined} onClick={() => void send()} disabled={sending || up.uploading || (!v.trim() && !up.ready.length) || disabled} className="rounded-full disabled:opacity-25">{sending ? <Spinner className="size-4" /> : <ArrowUpIcon className="size-4" strokeWidth={2.25} />}</InputGroupButton>
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
    <CodeBlock size="sm" className="mt-1 max-w-[640px] bg-muted/70 ring-0">
      <CodeBlockContent className="max-h-80 overflow-auto type-code">
        {lines.map((l, n) => <CodeBlockLine key={n} variant={l.startsWith("+") ? "added" : l.startsWith("-") ? "removed" : "default"} className={cn(l.startsWith("+") && "bg-success/10 after:text-success", l.startsWith("✓") && "text-success")}>{l.replace(/^[+-] /, "")}</CodeBlockLine>)}
      </CodeBlockContent>
    </CodeBlock>
  )
}
/** a command and what it printed, as a terminal */
function Terminal({ cmd, out, failed }: { cmd: string; out: string; failed: boolean }) {
  return (
    <div className={cn("mt-1 mb-1.5 max-w-[640px] overflow-hidden rounded-xl bg-[#1c1c1e] font-mono text-[12px] leading-[18px] text-[#e5e5ea] ring-1 ring-black/5 dark:bg-black/40 dark:ring-white/10", failed && "ring-destructive/40")} data-terminal>
      <div className="whitespace-pre-wrap break-all border-b border-white/10 px-3 py-2"><span className="select-none text-[#30d158]">$ </span>{cmd}</div>
      {out && <pre className={cn("max-h-72 overflow-auto whitespace-pre-wrap break-all px-3 py-2 text-[#aeaeb2]", failed && "text-[#ff8a80]")}>{out}</pre>}
    </div>
  )
}
function PlanList({ plan }: { plan: [string, boolean][] }) {
  return (
    <ul className="mt-1 mb-1.5 flex max-w-[640px] flex-col gap-1 rounded-xl bg-muted/60 px-3 py-2.5 type-callout" data-plan>
      {plan.map(([t, done], n) => <li key={n} className="flex items-start gap-2">{done ? <CircleCheckIcon className="mt-[3px] size-3.5 shrink-0 text-success" /> : <CircleIcon className="mt-[3px] size-3.5 shrink-0 text-tertiary" />}<span className={cn(done ? "text-muted-foreground" : "text-foreground")}>{t}</span></li>)}
    </ul>
  )
}
function Step({ e, live, last }: { e: any[]; live: boolean; last: boolean }) {
  const t = useT()
  if (e[0] === "a") return <div className="py-1 type-callout text-foreground/80" data-narration><Markdown text={e[1]} /></div>
  if (e[0] === "th") {
    const thinking = live && last, preview = String(e[1] || "").trim().split("\n")[0]
    return (
      <Collapsible data-step data-step-kind="th">
        <CollapsibleTrigger className="group/step flex w-full min-w-0 items-center gap-2 rounded-md py-[3px] text-left type-callout text-muted-foreground hover:text-foreground">
          <span className="flex size-4 shrink-0 items-center justify-center"><BrainIcon className="size-4" strokeWidth={1.5} /></span>
          <span className={cn("shrink-0 text-foreground/85", thinking && "shimmer")}>{t(thinking ? "core:transcript.thinking" : "core:transcript.thought")}</span>
          <span className="min-w-0 truncate italic">{preview}</span>
          <ChevronRightIcon className="size-3.5 shrink-0 opacity-0 transition group-hover/step:opacity-100 group-aria-expanded/step:rotate-90 group-aria-expanded/step:opacity-100" />
        </CollapsibleTrigger>
        <CollapsibleContent><div className="mb-1.5 ml-[7px] min-w-0 break-words border-l-2 pl-4 text-muted-foreground" data-thought-body><Markdown text={e[1]} className="type-callout" /></div></CollapsibleContent>
      </Collapsible>
    )
  }
  const v = stepView(e), running = live && e[5] === 1, failed = e[8] === 1
  const diff = e[1] === "edit" && e[6] != null ? <span className="shrink-0 font-mono text-[12px] tabular-nums"><span className="text-success">+{e[6]}</span>{e[7] ? <span className="ml-1 text-destructive">−{e[7]}</span> : null}</span> : null
  const body = e[12]?.length ? <PlanList plan={e[12]} /> : v.I === TerminalIcon ? <Terminal cmd={v.full ?? v.subj} out={String(e[4] ?? "").trim()} failed={failed} /> : e[4] ? <Detail text={e[4]} /> : null
  const head = (
    <>
      <span className="flex size-4 shrink-0 items-center justify-center">{running ? <Spinner className="size-3.5 text-blue" /> : failed ? <CircleXIcon className="size-4 text-destructive" strokeWidth={1.75} /> : <v.I className="size-4" strokeWidth={1.5} />}</span>
      <span className={cn("max-w-[60%] shrink-0 truncate text-foreground/85", running && "shimmer", failed && "text-destructive")}>{v.verb}</span>
      {v.subj && <span className={cn("min-w-0 truncate", v.mono && "font-mono text-[12px]")} title={v.full}>{v.subj}</span>}
      {diff}
      {failed && <span className="shrink-0 rounded-md bg-destructive/10 px-1.5 text-[11px] leading-[18px] text-destructive">{t("core:transcript.failed")}</span>}
    </>
  )
  const row = "flex w-full min-w-0 items-center gap-2 rounded-md py-[3px] text-left type-callout text-muted-foreground"
  if (!body) return <div data-step data-step-kind={e[1]} data-running={running || undefined} className={row}>{head}</div>
  return (
    <Collapsible data-step data-step-kind={e[1]} data-running={running || undefined} data-failed={failed || undefined}>
      <CollapsibleTrigger className={cn(row, "group/step hover:text-foreground")}>{head}<ChevronRightIcon className="size-3.5 shrink-0 opacity-0 transition group-hover/step:opacity-100 group-aria-expanded/step:rotate-90 group-aria-expanded/step:opacity-100" /></CollapsibleTrigger>
      <CollapsibleContent className="pl-6">{body}</CollapsibleContent>
    </Collapsible>
  )
}
/** what the agent is doing right now, in a few words */
function Doing({ evs }: { evs: any[] }) {
  const t = useT()
  const run = evs.findLast((e) => e[0] === "tool" && e[5] === 1)
  if (run) { const v = stepView(run); return <><span className="shrink-0">{v.verb}</span>{v.subj && <span className={cn("min-w-0 truncate text-muted-foreground", v.mono && "font-mono text-[11.5px]")}>{v.subj}</span>}</> }
  const last = evs[evs.length - 1]
  return <span>{t(!last ? "core:transcript.preparing" : last[0] === "th" ? "core:transcript.thinking" : last[0] === "a" ? "core:transcript.replying" : last[0] === "ask" ? "core:transcript.awaitingApproval" : "core:transcript.processing")}</span>
}
/* a folded block stays as the reader left it, across re-renders and remounts */
const workOpen = new Map<string, boolean>()
function WorkBlock({ k, g, live }: { k: string; g: Seg; live: boolean }) {
  const t = useT()
  const key = k + ":" + g.idx
  const [open, setOpenS] = useState(() => workOpen.get(key) ?? live), [all, setAll] = useState(false)
  useEffect(() => { if (!workOpen.has(key)) setOpenS(live) }, [live, key])
  const setOpen = (v: boolean) => { workOpen.set(key, v); setOpenS(v) }
  const now = useNow(live)
  const stats = workStats(g.evs), LIM = 12
  const hidden = !all && g.evs.length > LIM && live ? g.evs.length - LIM : 0
  const ms = live ? now - (g.t0 ?? now) : (g.t1 ?? 0) - (g.t0 ?? 0)
  const cancelled = g.end === "cancelled"
  return (
    <Collapsible open={open} onOpenChange={setOpen} data-work={live ? "live" : "done"} className="anim-in">
      <CollapsibleTrigger className="group/work flex w-full min-w-0 items-center gap-2 rounded-lg py-1 text-left type-callout">
        {live ? <span className="relative flex size-4 shrink-0 items-center justify-center"><span className="absolute size-2 rounded-full bg-blue dot-running" /></span>
          : cancelled ? <CircleSlashIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.75} /> : <CircleCheckIcon className="size-4 shrink-0 text-success" strokeWidth={1.75} />}
        <span className={cn("shrink-0 font-medium", live ? "text-blue" : "text-foreground/85")}>{live ? <span className="shimmer">{t("core:transcript.working")}</span> : t(cancelled ? "core:transcript.interrupted" : "core:transcript.handled")}</span>
        {(ms > 0 || live) && <span className="shrink-0 tabular-nums text-muted-foreground" data-work-clock>{live ? clock(ms) : fmtDur(ms)}</span>}
        {stats.length > 0 && <span className="min-w-0 truncate text-muted-foreground">· {stats.join(" · ")}</span>}
        <ChevronRightIcon className="ml-auto size-4 shrink-0 text-muted-foreground transition group-aria-expanded/work:rotate-90" />
      </CollapsibleTrigger>
      <CollapsibleContent className="h-(--collapsible-panel-height) overflow-hidden transition-[height] duration-200 data-ending-style:h-0 data-starting-style:h-0">
        <div className="ml-[7px] mt-1 flex flex-col border-l pl-4">
          {hidden > 0 && <button type="button" onClick={() => setAll(true)} className="self-start py-1 type-footnote text-muted-foreground hover:text-foreground" data-show-steps>{t("core:transcript.moreSteps", { count: hidden })}</button>}
          {g.evs.map((x, j) => j < hidden ? null : <Step key={j} e={x} live={live} last={j === g.evs.length - 1} />)}
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
    <div className="overflow-hidden rounded-xl border bg-card anim-in" data-files-changed>
      <div className="flex items-center gap-2 border-b bg-muted/40 px-3.5 py-2 type-footnote"><FileDiffIcon className="size-3.5 text-muted-foreground" /><span className="font-medium text-foreground">{t("core:transcript.filesChanged", { count: files.length })}</span><span className="font-mono tabular-nums"><span className="text-success">+{add}</span> <span className="text-destructive">−{del}</span></span></div>
      <ul className="flex flex-col py-1">{files.slice(0, 12).map(([p, a, d]) => (
        <li key={p} className="flex min-w-0 items-center gap-2 px-3.5 py-1 font-mono text-[12px]" title={p}><span className="shrink-0 text-foreground">{base(p)}</span><span className="min-w-0 truncate text-muted-foreground">{p.split("/").slice(-4, -1).join("/")}</span><span className="ml-auto shrink-0 tabular-nums"><span className="text-success">+{a}</span> <span className="text-destructive">−{d}</span></span></li>
      ))}{files.length > 12 && <li className="px-3.5 py-1 type-footnote text-muted-foreground">{t("core:transcript.moreFiles", { count: files.length - 12 })}</li>}</ul>
    </div>
  )
}
function ForkBtn({ k, at }: { k: string; at: number }) {
  const t = useT()
  return (
    <Tooltip>
      <TooltipTrigger render={<Button variant="ghost" size="icon-xs" aria-label={t("core:transcript.fork")} data-fork={at} onClick={() => openSheet("fork", { k, at })}
        className="shrink-0 rounded-full text-muted-foreground opacity-0 transition-opacity duration-150 group-hover/msg:opacity-100 focus-visible:opacity-100 [@media(hover:none)]:opacity-100" />}><GitForkIcon /></TooltipTrigger>
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
        <span className="min-w-0 truncate">{who}{head}</span><ChevronRightIcon className="size-3.5 shrink-0 transition-transform group-data-[panel-open]/brief:rotate-90" />
      </CollapsibleTrigger>
      <CollapsibleContent><div className="mt-1.5 max-h-80 overflow-auto break-words rounded-lg border px-3 py-2 text-muted-foreground" data-brief-body><Markdown text={text} className="type-footnote" /></div></CollapsibleContent>
    </Collapsible>
  )
}
/** range = [from, to) into the session's tuples: only that part of the conversation (one reply in the work log) */
type Seg = { t: string; evs: any[]; idx: number; t0?: number; t1?: number; end?: string; files?: [string, number, number][] }
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
          <BubbleContent className="min-w-0 break-words rounded-2xl px-3.5 py-2 type-body text-foreground">{e[2] && <div className="mb-0.5 type-label text-muted-foreground">{e[2]}</div>}<Markdown text={e[1]} /></BubbleContent>
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
    <div className="ml-auto max-w-[85%] rounded-2xl bg-muted px-3.5 py-2.5 anim-in" data-merged>
      <div className="mb-1 flex items-center gap-1.5 type-label text-muted-foreground"><InboxIcon className="size-3.5" />{t("core:transcript.merged", { count: e[1].length })}</div>
      {e[1].map((x: string) => <Markdown key={x} text={x} className="type-body" />)}
    </div>
  )
  if (g.t === "hi") return <div className="flex gap-2 rounded-xl bg-muted/50 px-3 py-2 type-callout text-muted-foreground" data-handoff><CornerDownRightIcon className="mt-0.5 size-4 shrink-0" strokeWidth={1.5} /><div className="min-w-0 flex-1"><Brief text={e[3] ?? e[1]} label={t("core:transcript.steveBrief")} /><Files files={e[2]} className="mt-1.5" /></div></div>
  if (g.t === "ho") return (
    <div className="flex items-center gap-3 type-footnote text-muted-foreground"><span className="h-px flex-1 bg-border" />
      <button type="button" onClick={() => go("#/sessions/" + e[2])} className="inline-flex items-center gap-1 text-blue hover:underline">{e[1]}<ArrowUpRightIcon className="size-3" /></button><span className="h-px flex-1 bg-border" /></div>
  )
  if (g.t === "ask") return <TranscriptPermission e={e} />
  if (g.t === "away") return <div className="flex items-center gap-2 type-footnote text-muted-foreground" data-away><CornerDownRightIcon className="size-3.5 shrink-0" strokeWidth={1.5} />{t("core:transcript.away")}</div>
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
      segs.push(work = { t: "work", evs: buf.map((x) => x[0]), idx: buf[0][1], t0: t0 || ts[buf[0][1]], t1: end?.[1] ?? ts[lastIdx + 1] ?? ts[lastIdx], end: end?.[0] })
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
  if (!s.ev.length && !range) return <div className="type-callout text-muted-foreground">{t(s.st === "run" ? "core:transcript.starting" : "core:transcript.empty")}</div>
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
  const turn = s.ev.slice(i + 1), t0 = (i >= 0 ? s.ts[i] : s.ts[0]) || now
  const end = turn.findLast((e) => e[0] === "end"), endAt = end ? s.ts[s.ev.lastIndexOf(end)] : 0
  const stats = workStats(turn)
  const tone = run ? "run" : s.st === "wait" ? "wait" : end?.[1] === "cancelled" ? "stop" : s.st === "ended" ? "ended" : "done"
  const label = t(`core:runBar.${tone}`)
  const what = run ? <Doing evs={turn} /> : tone === "wait" ? t("core:runBar.waitWhat") : stats.join(" · ")
  return (
    <div role="status" aria-live="polite" data-runbar={tone}
      className={cn("mb-2 flex min-h-10 items-center gap-2.5 rounded-xl border px-3.5 py-2 type-footnote transition-colors",
        tone === "run" ? "border-blue/25 bg-blue/[0.06]" : tone === "wait" ? "border-attention/35 bg-attention/[0.07]" : "bg-muted/40")}>
      {run ? <span className="relative flex size-4 shrink-0 items-center justify-center"><span className="absolute size-4 animate-ping rounded-full bg-blue/25" /><span className="size-2 rounded-full bg-blue" /></span>
        : tone === "wait" ? <StatusDot s="wait" className="mx-1" /> : tone === "done" ? <CircleCheckIcon className="size-4 shrink-0 text-success" strokeWidth={1.75} /> : tone === "stop" ? <CircleSlashIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.75} /> : <StatusDot s="ended" className="mx-1" />}
      <span className={cn("shrink-0 font-medium", tone === "run" ? "text-blue" : tone === "wait" ? "text-attention" : "text-foreground/85")}>{label}</span>
      {what && <span className={cn("flex min-w-0 items-baseline gap-1.5 truncate", run ? "text-foreground/85" : "text-muted-foreground")} data-doing>{what}</span>}
      <span className="ml-auto shrink-0 tabular-nums text-muted-foreground">{run ? clock(now - t0) : endAt && t0 ? t("core:runBar.took", { dur: fmtDur(endAt - t0) }) : ""}</span>
      {run && onStop && <Button size="xs" variant="outline" className="shrink-0 bg-background" onClick={onStop} data-interrupt><SquareIcon className="fill-current" />{t("core:runBar.interrupt")}</Button>}
    </div>
  )
}

/* a session as a block inside the item's work log */
const expanded = new Set<string>(), collapsedLive = new Set<string>()
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
    <div data-session-block={k}>
      <div className="group/sess flex min-h-9 items-center gap-2">
        <span className="flex size-5 shrink-0 items-center justify-center"><SquareTerminalIcon className="size-4 text-muted-foreground" strokeWidth={1.5} /></span>
        <button type="button" onClick={toggle} className="flex min-w-0 flex-1 items-center gap-2.5 text-left" aria-expanded={open}>
          <span className="shrink-0 type-callout font-medium">{t("core:session.no", { n: s.n })}</span>
          <Status s={s.st} sess />
          <span className="min-w-0 truncate type-footnote text-muted-foreground">{open ? `${st.devices.find((d) => d.id === s.device)?.name ?? "Hub"} · ${s.h} · ${s.m} · ${s.dur}${s.ctx ? t("core:session.context", { pct: s.ctx }) : ""}` : lastA ? lastA[1] : s.st === "run" ? t("core:runBar.run") : ""}</span>
          <span className="ml-auto shrink-0 type-footnote text-muted-foreground">{t(open ? "core:session.collapse" : "core:session.expand")}</span>
        </button>
        <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" onClick={() => go("#/sessions/" + k)} data-enter-session={k} aria-label={t("core:session.enter")} className="text-muted-foreground" />}><ArrowUpRightIcon /></TooltipTrigger><TooltipContent>{t("core:session.enter")}</TooltipContent></Tooltip>
      </div>
      {open && <div className="mb-2 ml-[9px] mt-1 border-l pl-[19px]"><Transcript k={k} compact={!live} tail={4} skip={skip?.length ? skip : undefined} /></div>}
    </div>
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
    <div className="mb-1.5 ml-[9px] border-l pl-[19px]" data-reply={r.s}>
      <div className="flex min-h-7 min-w-0 items-center gap-1.5 type-footnote text-muted-foreground">
        <BotAvatar k={who} size={16} /><span className="shrink-0 font-medium text-foreground/80">{botName(who)}</span>
        <span className="shrink-0">{t("core:session.replyNo", { n: s.n })}</span>
        {stats.length > 0 && <span className="min-w-0 truncate" data-reply-stats>· {stats.join(" · ")}</span>}
        {r.live && !open && <Spinner className="size-3 shrink-0" />}
        <button type="button" onClick={() => setOpen(!open)} aria-expanded={open} className="ml-auto shrink-0 hover:text-foreground" data-reply-toggle>{t(open ? "core:session.collapseWork" : "core:session.expandWork")}</button>
        <Tooltip><TooltipTrigger render={<Button variant="ghost" size="icon-xs" onClick={() => go("#/sessions/" + r.s)} aria-label={t("core:session.enter")} className="text-muted-foreground" />}><ArrowUpRightIcon /></TooltipTrigger><TooltipContent>{t("core:session.enter")}</TooltipContent></Tooltip>
      </div>
      {open ? <div className="pb-2 pt-1"><Transcript k={r.s} compact={!r.live} range={[r.from, r.to]} /></div>
        : r.text ? <div className="line-clamp-6 whitespace-pre-wrap break-words pb-1 type-body text-foreground" data-reply-text><Markdown text={r.text} /></div> : null}
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
    <CardHeader className="flex min-w-0 flex-col gap-2 pt-3.5 pb-3">
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
      {w.busy && <div role="status" className="col-span-full flex items-center gap-1.5 type-footnote text-muted-foreground" data-agent-busy><span className="size-1.5 shrink-0 animate-pulse rounded-full bg-blue" aria-hidden />{t("core:card.agentBusy")}</div>}
    </CardHeader>
  )
  const foot = (c: ReactNode) => <CardFooter className="flex-wrap justify-end gap-2 border-t bg-muted/20 px-4 py-2.5!">{c}</CardFooter>
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
  /* a typed answer is not one of the questionnaire's answer controls, so its validation would block the submit; send it directly */
  else if (w.type === "ask") { const typed = free || !w.opts?.length; body = <>
    {head(null)}
    <Questionnaire className="gap-0" onSubmit={(ev) => { ev.preventDefault(); const v = String(new FormData(ev.currentTarget).get("answer") ?? ""); if (v) void act("answer", v) }}>
      <QuestionnaireItem name="answer" required={!typed} className={cn("gap-3 px-(--card-spacing) pb-4", w.busy ? "mt-2" : "-mt-2")}>
        <QuestionnaireTitle className="mb-1! break-words type-headline">{w.q}</QuestionnaireTitle>
        {!free && !!w.opts?.length && <QuestionnaireChoices className="-mx-2 gap-0.5">
          {w.opts.map((o) => <QuestionnaireChoice key={o} value={o} className="min-h-10 rounded-lg border-transparent px-2 py-2.5 hover:bg-accent data-checked:border-transparent data-checked:bg-blue/5">{o}</QuestionnaireChoice>)}
        </QuestionnaireChoices>}
        {typed && <Textarea autoFocus className="min-h-16 rounded-lg" placeholder={t("core:card.answerPlaceholder")} value={txt} onChange={(e) => setTxt(e.target.value)} data-free-answer />}
      </QuestionnaireItem>
      <QuestionnaireActions className="flex justify-end gap-2 border-t px-3 py-2.5">{!!w.opts?.length && <Button type="button" variant="ghost" className="mr-auto text-muted-foreground" onClick={() => setFree(!free)}>{t(free ? "core:card.fromOptions" : "core:card.ownAnswer")}</Button>}{typed
        ? <Button type="button" data-answer disabled={busy || !txt.trim()} onClick={() => void act("answer", txt.trim())}>{t("core:card.answer")}</Button>
        : <QuestionnaireSubmit data-answer disabled={busy}>{t("core:card.answer")}</QuestionnaireSubmit>}</QuestionnaireActions>
    </Questionnaire>
  </> }
  return <Card size="sm" className="gap-0 py-0" data-decision={w.type} data-decision-wrap={i.id || w.decision} aria-busy={busy}>{busy && <div role="status" className="px-4 py-2 type-footnote text-muted-foreground">{t("core:card.submitting")}</div>}{body}</Card>
}

export function ItemRef({ id }: { id: string }) {
  const i = st.items.find((x) => x.id === id)
  if (!i) return null
  return (
    <UIItem variant="outline" size="sm" render={<button type="button" onClick={() => go("#/items/" + id)} />} data-item-ref={id} className="mt-3 w-full max-w-[460px] rounded-xl bg-card text-left hover:bg-accent anim-in">
      <ItemMedia variant="icon" className="rounded-lg bg-muted text-muted-foreground"><ListTodoIcon className="size-4" strokeWidth={1.5} /></ItemMedia>
      <ItemContent className="min-w-0 gap-0.5">
        <ItemTitle className="w-full min-w-0 type-body"><span className="min-w-0 truncate">{i.t}</span></ItemTitle>
        <ItemDescription className="truncate type-footnote"><span className="font-mono tabular-nums">{i.id}</span> · {botName(i.b)} · {i.now}</ItemDescription>
      </ItemContent>
      <ItemActions><Status s={i.st} /></ItemActions>
    </UIItem>
  )
}
