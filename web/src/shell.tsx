/* The app's frame, after affectUI's App Shell Flat: a gray sidebar beside the edge-to-edge page, split by one hairline.
 * Its head is the workspace (Steve's mark, the workspace's name, its menu) with the button that folds the sidebar to a
 * rail of icons; under it the Hub, the YOLO line, the running-status pill and search as a field with ⌘K; then the nav
 * groups; at the bottom the signed-in account (affectUI Profile Menu), the theme and the versions. A phone has no
 * sidebar: the tab bar at the bottom, and 我的 for the rest. */
import { useEffect, useState } from "react"
import { cn } from "cn"
import { api } from "@/api"
import { usePeopleOn, useMyAvatar } from "@/api/people"
import { HubSwitcher } from "@/hub-ui"
import { useT } from "@/i18n"
import { RunningNowPill } from "@/running-pill"
import { LOADED_WEB_VERSION, pendingCards, st } from "@/store"
import { getTheme, go, openSheet, setTheme, useScrollMore, type ThemePref } from "@/ui"
import { shortVersion } from "@/updates"
import { WorkspaceSwitcher } from "@/workspace-switcher"
import { YoloPill } from "@/yolo/switch"
import { ProfileMenu, type ProfileMenuItem } from "@/components/affect/profile-menu-clean"
import { Kbd } from "@/components/ui/kbd"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { Sidebar as UISidebar, SidebarContent, SidebarFooter, SidebarGroup, SidebarGroupContent, SidebarGroupLabel, SidebarHeader, SidebarMenu, SidebarMenuBadge, SidebarMenuButton, SidebarMenuItem, SidebarRail, useSidebar } from "@/components/ui/sidebar"
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip"
import {
  BookOpenIcon, BotIcon, ChartNoAxesColumnIcon, ChevronsUpDownIcon, ContactRoundIcon, FolderIcon, InboxIcon, KeyRoundIcon, ListTodoIcon, LogOutIcon, MailIcon,
  MessageCircleIcon, MessagesSquareIcon, MonitorIcon, MoonIcon, PanelLeftIcon, PlugIcon, RadioIcon, SearchIcon, ServerIcon, SettingsIcon, SquareTerminalIcon,
  SunIcon, UserIcon, UserRoundIcon, WorkflowIcon, BlocksIcon } from "lucide-react"

type NavGroup = "work" | "connect" | "automate"
const NAV_GROUPS: NavGroup[] = ["work", "connect", "automate"]
const NAV: { h: string; l: "chat" | "inbox" | "items" | "projects" | "sessions" | "stats" | "integrations" | "channels" | "mail" | "contacts" | "knowledge" | "bots" | "capabilities" | "auto" | "signals"; I: any; group: NavGroup }[] = [
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
  { h: "capabilities", l: "capabilities", I: BlocksIcon, group: "automate" },
  { h: "auto", l: "auto", I: WorkflowIcon, group: "automate" },
  { h: "signals", l: "signals", I: RadioIcon, group: "automate" },
]
/** a sidebar row, App Shell's: 30px, the name in the body colour, the open one on a quiet fill with its icon in indigo */
const ROW = "h-[30px] gap-2 rounded-lg px-1.5! type-callout text-foreground/80 hover:text-foreground data-active:font-medium data-active:text-foreground data-active:[&>svg]:text-primary"

/** folds the sidebar to its rail and back (⌘B does the same); in the rail it waits under Steve's mark until hovered */
function SidebarToggle({ className }: { className?: string }) {
  const t = useT(), { state, toggleSidebar } = useSidebar()
  const label = state === "collapsed" ? t("core:sidebar.expand") : t("core:sidebar.collapse")
  return (
    <Tooltip><TooltipTrigger render={<button type="button" aria-label={label} aria-keyshortcuts="Meta+B" onClick={toggleSidebar} data-sidebar-toggle={state} />}
      className={cn("flex size-7 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-[color,background-color,opacity] duration-120 ease-(--ease-out) hover:bg-sidebar-accent hover:text-foreground", className)}>
      <PanelLeftIcon className="size-4" strokeWidth={1.75} /></TooltipTrigger><TooltipContent side="right">{label} <Kbd>⌘B</Kbd></TooltipContent></Tooltip>
  )
}

/** the theme as one quiet button that steps light → dark → system; its tooltip names the current one */
function ThemeCycle({ className }: { className?: string }) {
  const t = useT(), pref = getTheme()
  const order: ThemePref[] = ["light", "dark", "system"], I = pref === "light" ? SunIcon : pref === "dark" ? MoonIcon : MonitorIcon
  const label = `${t("core:sidebar.appearance")} · ${t(`core:theme.${pref}`)}`
  return <Tooltip><TooltipTrigger render={<button type="button" aria-label={label} data-theme-cycle={pref} onClick={() => setTheme(order[(order.indexOf(pref) + 1) % order.length])} />}
    className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg text-muted-foreground transition-colors duration-120 ease-(--ease-out) hover:bg-sidebar-accent hover:text-foreground", className)}>
    <I className="size-4" strokeWidth={1.75} /></TooltipTrigger><TooltipContent side="top">{label}</TooltipContent></Tooltip>
}

type Me = { name: string; email: string | null; role: string }
/** who is signed in (GET /me), once per page: the name and email do not change with the workspace (the role does, and
 *  comes from the store's workspaces); a fixture or a Hub that does not answer leaves it unknown */
function useMe() {
  const [me, setMe] = useState<Me | null>(null)
  useEffect(() => { let on = true; api.me().then((r) => { if (on) setMe(r.user) }, () => {}); return () => { on = false } }, [])
  return me
}
const initial = (name: string) => (Array.from(name.trim())[0] ?? "").toUpperCase()
/** your picture (uploaded on 我) or your initial */
function Initials({ name, src, className }: { name: string; src?: string; className?: string }) {
  const c = initial(name)
  if (src) return <img src={src} alt="" aria-hidden className={cn("shrink-0 rounded-full object-cover", className)} data-account-avatar />
  return <span aria-hidden className={cn("flex shrink-0 items-center justify-center rounded-full bg-primary/12 font-semibold text-primary", className)}>{c || <UserIcon className="size-3.5" strokeWidth={1.75} />}</span>
}

/** The account at the bottom of the sidebar: who is signed in, and behind it (affectUI Profile Menu) the places that are
 *  theirs – 我, devices, accounts, settings – and signing out. Its row is marked while one of those places is open. */
function Account({ route, meOn }: { route: string; meOn: boolean }) {
  const t = useT(), me = useMe(), people = usePeopleOn(), pic = useMyAvatar(), { state, isMobile } = useSidebar()
  const [open, setOpen] = useState(false)
  const leave = (h: string) => { setOpen(false); go(h) }
  const name = me?.name || t("core:sidebar.account")
  /* the role in the workspace open now (a workspace switch does not reload the page), the instance role until it is known */
  const r = st.workspaces.find((w) => w.id === st.workspace?.id)?.role ?? me?.role ?? ""
  const role = r === "owner" || r === "member" ? t(`core:sidebar.role.${r}`) : r
  const place = (id: string, h: string, label: string, I: any, current: boolean): ProfileMenuItem =>
    ({ id, label, icon: <I strokeWidth={1.75} />, current, onSelect: () => leave("#/" + h), data: { "data-nav": h } })
  const items: ProfileMenuItem[] = [
    ...(people ? [place("me", "contacts/me", t("core:nav.me"), UserRoundIcon, meOn)] : []),
    place("devices", "devices", t("core:nav.devices"), ServerIcon, route === "devices"),
    place("accounts", "accounts", t("core:nav.accounts"), KeyRoundIcon, route === "accounts" || route === "vault"),
    place("settings", "settings", t("core:nav.settings"), SettingsIcon, route === "settings"),
    { id: "signout", label: t("pages:access.signOut"), icon: <LogOutIcon strokeWidth={1.75} />, data: { "data-account-sign-out": "" }, onSelect: async () => { setOpen(false); await api.logout().catch(() => {}); location.reload() } },
  ]
  const on = items.some((x) => x.current)
  return (
    <SidebarMenuItem className="min-w-0 flex-1">
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger render={<SidebarMenuButton isActive={on} tooltip={open ? undefined : name} className={cn(ROW, "h-8 font-medium")} data-account={me ? r : ""} />}>
          <Initials name={me?.name ?? ""} src={pic} className="size-5 type-label leading-none" />
          <span className="min-w-0 flex-1 truncate">{name}</span>
          <ChevronsUpDownIcon className="size-3.5! text-tertiary" />
        </PopoverTrigger>
        <PopoverContent side={state === "collapsed" && !isMobile ? "right" : "top"} align="start" sideOffset={6}
          className="w-60 gap-0 rounded-xl bg-transparent p-0 shadow-none ring-0" data-account-menu>
          <ProfileMenu className="max-w-none" name={name} email={me?.email || role} menuLabel={t("core:sidebar.account")}
            avatar={<Initials name={me?.name ?? ""} src={pic} className="flex! type-callout" />} items={items} />
        </PopoverContent>
      </Popover>
    </SidebarMenuItem>
  )
}

export function Sidebar({ route: full }: { route: string }) {
  const t = useT()
  const route = full.split("?")[0] // #/knowledge?mem=… still highlights 知识
  const n = pendingCards().length
  const meOn = route === "contacts" && location.hash.startsWith("#/contacts/me")
  /* what is in progress is the running-status pill's, under the header; the inbox keeps its count of open cards */
  const cnt: Record<string, number> = { inbox: n }
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
      <SidebarMenuButton isActive={on} tooltip={l} render={<a href={"#/" + h} data-nav={h || "chat"} aria-current={on ? "page" : undefined} />} className={ROW}>
        <I className="size-4! text-muted-foreground transition-colors" strokeWidth={1.75} /><span>{l}</span>
      </SidebarMenuButton>
      {!!cnt[h] && (h === "inbox"
        ? <SidebarMenuBadge className="h-4.5 min-w-4.5 rounded-full bg-attention px-1.5 type-label font-semibold tabular-nums text-white! peer-hover/menu-button:text-white!">{cnt[h]}</SidebarMenuBadge>
        : <SidebarMenuBadge className="type-footnote tabular-nums text-muted-foreground">{cnt[h]}</SidebarMenuBadge>)}
    </SidebarMenuItem>
  )
  return (
    <UISidebar collapsible="icon" className="border-r border-sidebar-border [&_[data-slot=sidebar-inner]]:backdrop-blur-2xl">
      <SidebarHeader className="gap-1 px-2 pt-2.5 pb-1">
        {/* the head: the workspace and its menu, and the button that folds the sidebar. In the rail the button lies over
         *  Steve's mark and shows when the head is hovered or focused, as App Shell's does */}
        <div className="group/head relative flex min-h-8 items-center gap-1" data-sidebar-head>
          <SidebarMenu className="min-w-0 flex-1"><WorkspaceSwitcher /></SidebarMenu>
          <SidebarToggle className="group-data-[collapsible=icon]:absolute group-data-[collapsible=icon]:inset-0.5 group-data-[collapsible=icon]:size-7 group-data-[collapsible=icon]:bg-sidebar group-data-[collapsible=icon]:opacity-0 group-data-[collapsible=icon]:group-hover/head:opacity-100 group-data-[collapsible=icon]:focus-visible:opacity-100" />
        </div>
        <SidebarMenu>
          <HubSwitcher />
          {st.yolo && <SidebarMenuItem className="group-data-[collapsible=icon]:hidden" data-sidebar-yolo><YoloPill className="h-[30px] w-full gap-[13px] rounded-lg pr-2 pl-[11px] hover:bg-sidebar-accent" /></SidebarMenuItem>}
          <SidebarMenuItem className="group-data-[collapsible=icon]:hidden" data-sidebar-running><RunningNowPill className="steve-rn-side" /></SidebarMenuItem>
          {/* search as App Shell's field: a lifted card with its shortcut; in the rail only its icon */}
          <SidebarMenuItem className="mt-1.5 group-data-[collapsible=icon]:mt-0">
            <SidebarMenuButton tooltip={t("core:sidebar.searchTip")} onClick={() => openSheet("command")} aria-keyshortcuts="Meta+K"
              className="h-[30px] gap-2 rounded-lg bg-card px-2! type-callout text-muted-foreground shadow-[var(--lift)] transition-[box-shadow,color] duration-120 hover:bg-card hover:text-foreground hover:shadow-[var(--lift-hover)] group-data-[collapsible=icon]:bg-transparent group-data-[collapsible=icon]:shadow-none group-data-[collapsible=icon]:hover:bg-sidebar-accent" data-open-command>
              <SearchIcon className="size-4! text-tertiary" strokeWidth={1.75} /><span className="flex-1">{t("core:sidebar.search")}</span><Kbd className="h-4.5 bg-transparent px-1 type-label text-tertiary shadow-[inset_0_0_0_1px_var(--border)] group-data-[collapsible=icon]:hidden">⌘K</Kbd>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      {/* a short window gives the groups less room than they need: they scroll between header and footer. The edge
       *  with more beyond it fades and the footer keeps a hairline, so the lower groups read as scrolled, not cut off */}
      <SidebarContent ref={nav} className="gap-0 [scrollbar-width:thin]! data-[more~=end]:mask-b-from-[calc(100%-2rem)] data-[more~=start]:mask-t-from-[calc(100%-2rem)]" data-sidebar-nav data-more={more}>
        {NAV_GROUPS.map((group) => <SidebarGroup key={group} className="px-2 py-1">
          <SidebarGroupLabel className="h-7 px-1.5 type-footnote font-medium text-tertiary">{t(`core:navGroup.${group}`)}</SidebarGroupLabel>
          <SidebarGroupContent><SidebarMenu className="gap-px">{NAV.filter((x) => x.group === group).map((x) => item(x.h, t(`core:nav.${x.l}`), x.I, (route === x.h && !(x.h === "contacts" && meOn)) || (x.h === "contacts" && (route === "orgs" || route === "teams"))))}</SidebarMenu></SidebarGroupContent>
        </SidebarGroup>)}
      </SidebarContent>
      {/* the foot: the account (its menu holds 我, devices, accounts, settings) beside the theme, and the versions under
       *  it – short, so the nav groups above keep their room on a laptop screen */}
      <SidebarFooter className="gap-0.5 border-t border-sidebar-border px-2 pt-1.5 pb-2">
        <SidebarMenu className="flex-row items-center gap-0.5 group-data-[collapsible=icon]:flex-col">
          <Account route={route} meOn={meOn} />
          <ThemeCycle className="group-data-[collapsible=icon]:hidden" />
        </SidebarMenu>
        {/* ST-21: this page's frontend and the Hub's backend; the full numbers are in 设置 · 关于 */}
        <a href="#/settings/about" title={t("core:sidebar.versions", { web: LOADED_WEB_VERSION, hub: st.version || t("pages:version.fetching") })} className="block truncate rounded-lg px-1.5 py-0.5 type-footnote tabular-nums text-muted-foreground hover:bg-sidebar-accent hover:text-foreground group-data-[collapsible=icon]:hidden" data-sidebar-versions>{t("core:sidebar.versions", { web: shortVersion(LOADED_WEB_VERSION), hub: st.version ? shortVersion(st.version) : t("core:sidebar.versionUnknown") })}</a>
        <HubUpdateNotice />
        {!!st.webVersion && st.webVersion !== LOADED_WEB_VERSION && <a href="#/settings/about" className="block rounded-lg px-1.5 py-1 type-footnote text-primary hover:bg-sidebar-accent group-data-[collapsible=icon]:hidden" data-sidebar-web-update>{t("core:sidebar.webUpdate")}</a>}
      </SidebarFooter>
      <SidebarRail />
    </UISidebar>
  )
}
/** A newer Steve release the Hub found on its own: leads to Settings · 关于, where it is installed. */
export function HubUpdateNotice() {
  const t = useT()
  if (!st.update) return null
  return <a href="#/settings/about" className="flex items-center gap-2 rounded-lg px-1.5 py-1 type-footnote text-primary hover:bg-sidebar-accent group-data-[collapsible=icon]:hidden" data-sidebar-hub-update={st.update.version}>
    <span className="size-1.5 shrink-0 rounded-full bg-attention" aria-hidden />{t("core:sidebar.hubUpdate", { version: st.update.version })}
  </a>
}
/** a phone's tab bar: 对话, 待处理 with its count, 事项, and 我的 for every other place */
export function MobileTabs({ route: full }: { route: string }) {
  const tr = useT()
  const route = full.split("?")[0]
  const n = pendingCards().length
  const tabs = [{ h: "", l: tr("core:nav.chat"), I: MessageCircleIcon }, { h: "inbox", l: tr("core:nav.inbox"), I: InboxIcon, b: n }, { h: "items", l: tr("core:nav.items"), I: ListTodoIcon }, { h: "more", l: tr("core:nav.mine"), I: UserIcon, dot: !!st.update }]
  const more = ["more", "projects", "sessions", "stats", "mail", "signals", "settings", "bots", "capabilities", "design", "auto", "personas", "contacts", "orgs", "teams", "accounts", "vault", "devices", "knowledge", "github", "yolo", "channels", "integrations"]
  return (
    <nav aria-label={tr("core:sidebar.tabs")} className="fixed inset-x-0 bottom-0 z-40 flex h-15 border-t border-sidebar-border bg-sidebar/95 pb-[env(safe-area-inset-bottom)] backdrop-blur-2xl backdrop-saturate-180 md:hidden" data-mobile-tabs>
      {tabs.map((t) => {
        const on = route === t.h || (t.h === "more" && more.includes(route))
        return (
          <a key={t.h} href={"#/" + t.h} data-tab={t.h || "chat"} aria-current={on ? "page" : undefined} className={cn("relative flex flex-1 flex-col items-center justify-center gap-0.5 type-label transition-[color,transform] duration-120 active:scale-95", on ? "text-primary" : "text-muted-foreground")}>
            <t.I className="size-5.5" strokeWidth={on ? 1.9 : 1.5} />{t.l}
            {t.b ? <span className="absolute left-1/2 top-1.5 ml-2 flex h-4 min-w-4 items-center justify-center rounded-full bg-attention px-1 type-label leading-none font-semibold tabular-nums text-white ring-2 ring-sidebar">{t.b}</span> : null}
            {"dot" in t && t.dot ? <span className="absolute left-1/2 top-2 ml-2.5 size-2 rounded-full bg-attention ring-2 ring-sidebar" data-tab-update-dot /> : null}
          </a>
        )
      })}
    </nav>
  )
}
