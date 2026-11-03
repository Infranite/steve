import { Toaster, toast as sonnerToast } from "sonner"
import { useEffect, useState } from "react"
import { TooltipProvider } from "@/components/ui/tooltip"
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Command, CommandDialog, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList, CommandSeparator, CommandShortcut } from "@/components/ui/command"
import { BookOpenIcon, BotIcon, ChartNoAxesColumnIcon, Building2Icon, ContactRoundIcon, FolderIcon, GitPullRequestIcon, KeyRoundIcon, LockIcon, UsersRoundIcon, WorkflowIcon, InboxIcon, ListTodoIcon, MessageCircleIcon, MessagesSquareIcon, MoonIcon, PlugIcon, PlusIcon, RadioIcon, ServerIcon, SettingsIcon, SquareTerminalIcon, SunIcon } from "lucide-react"
import { st, useStore, start } from "@/store"
import { useRoute, Sidebar, MobileTabs, getSheet, resolvedTheme, openSheet, closeSheet, go, setTheme, StatusDot, ItemNo } from "@/ui"
import { ChatView, InboxView, ItemsView, ItemView, SessionsView, SessionView, MoreView, DesignView, ForkDialog, NewItemDialog, GraphDialog, LinkDialog, ImportDialog } from "@/views"
import { BotsView, BotView, NewBotDialog, DeleteBotDialog, SettingsView, TagDialog } from "@/manage"
import { DevicesView, DeviceView, PairDialog } from "@/devices"
import { ProjectsView, ProjectView, NewProjectDialog, NewWorkspaceDialog } from "@/projects"
import { Unwired } from "@/unwired"
import { ChannelsRoute } from "@/channels/channels"
import { IntegrationsRoute } from "@/integrations"
import { GitHubView } from "@/github/index"
import { KnowledgeView } from "@/knowledge"
import { SignalsView } from "@/signals"
import { AutoRoute } from "@/auto/automation"
import { AccountsRoute } from "@/accounts"
import { MailView } from "@/mail/list"; import { ThreadView } from "@/mail/thread"
import { VaultRoute } from "@/vault"
import { ContactsView, ContactView, MeView, OrgView, TeamView } from "@/people/routes"
import { Robot } from "@/robot"
import { HoldBanner } from "@/queue"
import { YoloBanner } from "@/yolo/banner"
import { YoloDialog } from "@/yolo/switch"
import { YoloDigestView } from "@/yolo/digest"
import { StatsRoute } from "@/stats/page"
import { people, usePeopleOn, type PartyOption } from "@/api/people"

/** ⌘K's 联系人 / 团队 group: the Hub's party search for the typed text (only where the contacts module is) */
function useParties(q: string): PartyOption[] {
  const on = usePeopleOn()
  const [xs, setXs] = useState<PartyOption[]>([])
  useEffect(() => {
    const t = q.trim(); if (!on || !t) { setXs([]); return }
    let live = true
    const h = setTimeout(() => { people.search(t).then((r) => live && setXs([...r.contacts, ...r.teams, ...r.orgs]), () => live && setXs([])) }, 150)
    return () => { live = false; clearTimeout(h) }
  }, [q, on])
  return xs
}
const PARTY_HREF: Record<PartyOption["type"], string> = { contact: "#/contacts/", org: "#/orgs/", team: "#/teams/" }
const PARTY_ICON: Record<PartyOption["type"], any> = { contact: ContactRoundIcon, org: Building2Icon, team: UsersRoundIcon }

/* ⌘K — jump to any page, item, project or session; quick actions */
function CommandMenu({ open }: { open: boolean }) {
  const [q, setQ] = useState("")
  const parties = useParties(open ? q : "")
  useEffect(() => {
    const h = (e: KeyboardEvent) => { if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") { e.preventDefault(); open ? closeSheet() : openSheet("command") } }
    window.addEventListener("keydown", h); return () => window.removeEventListener("keydown", h)
  }, [open])
  const run = (f: () => void) => { closeSheet(); f() }
  const pages: [string, string, any][] = [["", "对话", MessageCircleIcon], ["inbox", "待处理", InboxIcon], ["items", "事项", ListTodoIcon], ["projects", "项目", FolderIcon], ["sessions", "会话", SquareTerminalIcon], ["stats", "统计", ChartNoAxesColumnIcon], ["signals", "信号", RadioIcon], ["auto", "自动化", WorkflowIcon], ["integrations", "接入", PlugIcon], ["channels", "渠道", MessagesSquareIcon], ["contacts", "联系人", ContactRoundIcon], ["knowledge", "知识", BookOpenIcon], ["github", "GitHub", GitPullRequestIcon], ["accounts", "账号", KeyRoundIcon], ["vault", "保险箱", LockIcon], ["bots", "Bot", BotIcon], ["devices", "设备", ServerIcon], ["settings/general", "设置", SettingsIcon], ["settings/tags", "设置 · 标签", SettingsIcon], ["settings/workspace", "设置 · 工作空间", SettingsIcon]]
  return (
    <CommandDialog open={open} onOpenChange={(o) => !o && closeSheet()} title="搜索" description="跳转到页面、事项、项目或会话">
      <Command filter={(v, q) => (v.toLowerCase().includes(q.trim().toLowerCase()) ? 1 : 0)}>
      <CommandInput placeholder="搜索页面、事项编号、标题、项目或会话…" value={q} onValueChange={setQ} data-command-input />
      <CommandList className="max-h-[420px]">
        <CommandEmpty>没有匹配的结果</CommandEmpty>
        <CommandGroup heading="操作">
          <CommandItem onSelect={() => run(() => { go("#/items"); openSheet("newItem") })}><PlusIcon />新事项<CommandShortcut>N</CommandShortcut></CommandItem>
          <CommandItem onSelect={() => run(() => { go("#/projects"); openSheet("newProject") })}><FolderIcon />新建项目</CommandItem>
          <CommandItem onSelect={() => run(() => { go("#/bots"); openSheet("newBot") })} data-cmd-newbot><BotIcon />新建 Bot</CommandItem>
          <CommandItem onSelect={() => run(() => setTheme(resolvedTheme() === "dark" ? "light" : "dark"))}>{resolvedTheme() === "dark" ? <SunIcon /> : <MoonIcon />}切换到{resolvedTheme() === "dark" ? "浅色" : "深色"}</CommandItem>
        </CommandGroup>
        <CommandSeparator />
        <CommandGroup heading="页面">{pages.map(([h, l, I]) => <CommandItem key={h} value={"页面 " + l} onSelect={() => run(() => go("#/" + h))}><I />{l}</CommandItem>)}</CommandGroup>
        <CommandSeparator />
        {parties.length > 0 && <><CommandGroup heading="联系人 / 团队">{parties.map((p) => { const I = PARTY_ICON[p.type] ?? ContactRoundIcon; return <CommandItem key={p.type + p.id} value={"联系人 " + p.id + " " + p.name + " " + q} onSelect={() => run(() => go(PARTY_HREF[p.type] + p.id))} data-cmd-party={p.id}><I /><span className="truncate">{p.name}</span>{p.sub && <CommandShortcut className="max-w-[45%] truncate">{p.sub}</CommandShortcut>}</CommandItem> })}</CommandGroup><CommandSeparator /></>}
        <CommandGroup heading="项目">{st.projects.map((p) => <CommandItem key={p.id} value={"项目 " + p.name + " " + p.path} onSelect={() => run(() => go("#/projects/" + p.id))}><FolderIcon />{p.name}<CommandShortcut className="font-mono">{p.path.replace(/^\/Users\/[^/]+/, "~")}</CommandShortcut></CommandItem>)}</CommandGroup>
        <CommandSeparator />
        <CommandGroup heading="Bot">{Object.keys(st.bots).map((k) => <CommandItem key={k} value={"Bot " + st.bots[k].n + " " + st.bots[k].role} onSelect={() => run(() => go("#/bots/" + k))}><BotIcon />{st.bots[k].main ? st.bots[k].n : st.bots[k].n + " Bot"}<CommandShortcut>{st.bots[k].role.slice(0, 12)}</CommandShortcut></CommandItem>)}</CommandGroup>
        <CommandSeparator />
        <CommandGroup heading="对话">{st.thread.filter((m) => m.k !== "thinking" && m.t).slice(-30).map((m, n) => <CommandItem key={n} value={"对话 " + m.t} onSelect={() => run(() => go("#/"))}><MessageCircleIcon /><span className="truncate">{m.t}</span></CommandItem>)}</CommandGroup>
        <CommandSeparator />
        <CommandGroup heading="事项">{st.items.map((i) => <CommandItem key={i.id} value={i.id + " " + i.t} onSelect={() => run(() => go("#/items/" + i.id))} data-cmd-item={i.id}><StatusDot s={i.st} /><ItemNo id={i.id} className="w-14" /><span className="truncate">{i.t}</span></CommandItem>)}</CommandGroup>
        <CommandSeparator />
        <CommandGroup heading="会话">{Object.keys(st.S).map((k) => { const s = st.S[k]; return <CommandItem key={k} value={"会话 " + s.title + " " + (s.item ?? "")} onSelect={() => run(() => go("#/sessions/" + k))}><SquareTerminalIcon /><span className="truncate">{s.title}</span>{s.item && <CommandShortcut className="font-mono">{s.item} · #{s.n}</CommandShortcut>}</CommandItem> })}</CommandGroup>
      </CommandList>
      </Command>
    </CommandDialog>
  )
}

/** an old route (#/auto/acc, #/auto/space, #/personas): replaced in history by its new place under #/channels */
function Redirect({ to }: { to: string }) {
  useEffect(() => { history.replaceState(null, "", location.pathname + location.search + to); window.dispatchEvent(new HashChangeEvent("hashchange")) }, [to])
  return null
}

function Splash({ children }: { children: React.ReactNode }) {
  return <div className="flex h-[100dvh] flex-col items-center justify-center gap-4 bg-background px-6 text-center text-foreground">{children}</div>
}
const UNWIRED = ["plugins"]

export default function App() {
  useStore()
  useEffect(() => start(), [])
  const [a = "", b, c] = useRoute()
  /* an “撤销” toast belongs to the page it was raised on */
  useEffect(() => { sonnerToast.dismiss() }, [a, b, c])
  if (st.login) return (
    <Splash><Robot k="steve" size={72} /><div><div className="type-title">Steve</div><div className="mt-1 type-callout text-muted-foreground">登录后继续</div></div>
      <Button onClick={() => { location.href = st.login + "?back=" + encodeURIComponent(location.pathname + location.hash) }} data-login>登录</Button></Splash>
  )
  if (!st.ready) return <Splash>{st.error ? <><div className="type-headline">连不上 Steve</div><div className="type-callout text-muted-foreground">{st.error}</div></> : <Spinner className="size-5 text-muted-foreground" />}</Splash>
  const view =
    a === "inbox" ? <InboxView /> :
    a === "items" ? (b ? <ItemView key={b} id={b} /> : <ItemsView />) :
    a === "projects" ? (b ? <ProjectView key={b} id={b} /> : <ProjectsView />) :
    a === "sessions" ? (b ? <SessionView key={b} k={b} /> : <SessionsView />) :
    a === "settings" ? <SettingsView /> :
    a === "bots" ? (b ? <BotView key={b} k={b} /> : <BotsView />) :
    a === "design" ? <DesignView /> :
    a.split("?")[0] === "knowledge" ? <KnowledgeView sub={b} id={c} /> :
    a === "devices" ? (b ? <DeviceView key={b} id={b} /> : <DevicesView />) :
    a.split("?")[0] === "github" ? <GitHubView repo={b?.split("?")[0]} /> :
    a === "integrations" ? <IntegrationsRoute id={b} /> :
    a === "channels" ? <ChannelsRoute b={b} id={c} /> :
    a === "auto" && (b === "acc" || b === "space") ? <Redirect to={`#/channels/${b}/${c ?? ""}`} /> : a === "personas" ? <Redirect to={"#/channels/personas" + (b ? "/" + b : "")} /> :
    a.split("?")[0] === "signals" ? <SignalsView /> : a === "auto" ? <AutoRoute b={b} c={c} /> :
    a === "mail" || a.startsWith("mail?") ? (b === "t" && c ? <ThreadView key={c} id={c} /> : <MailView />) :
    a === "accounts" || a.startsWith("accounts?") ? <AccountsRoute route={a} id={b} sub={c} /> :
    a === "vault" ? <VaultRoute id={b} /> :
    a === "yolo" ? <YoloDigestView window={b} /> :
    a === "stats" ? <StatsRoute b={b} c={c} /> :
    a === "contacts" ? (b === "me" ? <MeView /> : b ? <ContactView key={b} id={b} /> : <ContactsView />) : a === "orgs" ? (b ? <OrgView key={b} id={b} /> : <ContactsView />) : a === "teams" ? (b ? <TeamView key={b} id={b} /> : <ContactsView />) :
    UNWIRED.includes(a) ? <Unwired id={a} /> :
    a === "more" ? <MoreView /> : a === "c" && b ? <ChatView key={b} conv={decodeURIComponent(b)} /> : <ChatView />
  const sh = getSheet()
  return (
    <TooltipProvider delay={400}>
      <SidebarProvider className="h-[100dvh] min-h-0! bg-background text-foreground [--sidebar-width:15rem]">
        <Sidebar route={a} />
        <SidebarInset className="relative min-w-0 overflow-hidden" key={(st.workspace?.id ?? "") + "/" + a + "/" + (b ?? "") + "/" + (c ?? "")}><YoloBanner /><HoldBanner /><div className="min-h-0 flex-1 anim-page">{view}</div></SidebarInset>
        <MobileTabs route={a} />
        {sh?.kind === "fork" && <ForkDialog k={sh.payload.k} at={sh.payload.at} />}
        {sh?.kind === "newItem" && <NewItemDialog project={sh.payload?.project} />}
        {sh?.kind === "newBot" && <NewBotDialog />}
        {sh?.kind === "graph" && <GraphDialog k={sh.payload.k} />}
        {sh?.kind === "link" && <LinkDialog k={sh.payload.k} />}
        {sh?.kind === "import" && <ImportDialog />}
        {sh?.kind === "deleteBot" && <DeleteBotDialog k={sh.payload.k} />}
        {sh?.kind === "pair" && <PairDialog />}
        {sh?.kind === "tag" && <TagDialog id={sh.payload?.id} item={sh.payload?.item} name={sh.payload?.name} />}
        {sh?.kind === "newProject" && <NewProjectDialog />}
        {sh?.kind === "newWorkspace" && <NewWorkspaceDialog />}
        {sh?.kind === "yolo" && <YoloDialog />}
        <CommandMenu open={sh?.kind === "command"} />
        <Toaster visibleToasts={1} theme={resolvedTheme()} position="bottom-center" offset={84} mobileOffset={76} toastOptions={{ className: "!rounded-full !border-0 !bg-foreground !text-background !shadow-lg !py-2.5 !px-4 !w-auto !text-[13px] !font-medium !justify-center" }} />
      </SidebarProvider>
    </TooltipProvider>
  )
}
