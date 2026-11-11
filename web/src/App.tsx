import { lazyView } from "@/lazy"
import { ChatView } from "@/chat-view"
import { Toaster, toast as sonnerToast } from "sonner"
import { useEffect, useState } from "react"
import { TooltipProvider } from "@/components/ui/tooltip"
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Input } from "@/components/ui/input"
import { api, signIn } from "@/api"
import { API, currentHub, hello, hubBase, hubUrl, nativeBridge, rememberHub, setHub } from "@/hub"
import { HubChooser, HubLine, HubsDialog, HubsOffer, HubsOfferDialog, PairClaim, ScanButton, usePairRequest } from "@/hub-ui"
import { Command, CommandDialog, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList, CommandSeparator, CommandShortcut } from "@/components/ui/command"
import { BookOpenIcon, BotIcon, ChartNoAxesColumnIcon, Building2Icon, ContactRoundIcon, FolderIcon, GitPullRequestIcon, KeyRoundIcon, LockIcon, QrCodeIcon, UsersRoundIcon, WorkflowIcon, InboxIcon, ListTodoIcon, MessageCircleIcon, MessagesSquareIcon, MoonIcon, PlugIcon, PlusIcon, RadioIcon, ServerIcon, SettingsIcon, SquareTerminalIcon, SunIcon } from "lucide-react"
import { st, useStore, start } from "@/store"
import { useT } from "@/i18n"
import { useRoute, Sidebar, MobileTabs, getSheet, resolvedTheme, openSheet, closeSheet, go, setTheme, StatusDot, ItemNo } from "@/ui"
const InboxView = lazyView(() => import("@/views").then((m) => ({ default: m.InboxView })))
const ItemsView = lazyView(() => import("@/views").then((m) => ({ default: m.ItemsView })))
const ItemView = lazyView(() => import("@/views").then((m) => ({ default: m.ItemView })))
const SessionsView = lazyView(() => import("@/views").then((m) => ({ default: m.SessionsView })))
const SessionView = lazyView(() => import("@/views").then((m) => ({ default: m.SessionView })))
const MoreView = lazyView(() => import("@/views").then((m) => ({ default: m.MoreView })))
const DesignView = lazyView(() => import("@/views").then((m) => ({ default: m.DesignView })))
const ForkDialog = lazyView(() => import("@/views").then((m) => ({ default: m.ForkDialog })))
const NewItemDialog = lazyView(() => import("@/views").then((m) => ({ default: m.NewItemDialog })))
const GraphDialog = lazyView(() => import("@/views").then((m) => ({ default: m.GraphDialog })))
const LinkDialog = lazyView(() => import("@/views").then((m) => ({ default: m.LinkDialog })))
const ImportDialog = lazyView(() => import("@/views").then((m) => ({ default: m.ImportDialog })))
const BotsView = lazyView(() => import("@/manage").then((m) => ({ default: m.BotsView })))
const BotView = lazyView(() => import("@/manage").then((m) => ({ default: m.BotView })))
const NewBotDialog = lazyView(() => import("@/manage").then((m) => ({ default: m.NewBotDialog })))
const DeleteBotDialog = lazyView(() => import("@/manage").then((m) => ({ default: m.DeleteBotDialog })))
const SettingsView = lazyView(() => import("@/manage").then((m) => ({ default: m.SettingsView })))
const TagDialog = lazyView(() => import("@/manage").then((m) => ({ default: m.TagDialog })))
const DevicesView = lazyView(() => import("@/devices").then((m) => ({ default: m.DevicesView })))
const DeviceView = lazyView(() => import("@/devices").then((m) => ({ default: m.DeviceView })))
const PairDialog = lazyView(() => import("@/devices").then((m) => ({ default: m.PairDialog })))
const PairDeviceDialog = lazyView(() => import("@/access-settings").then((m) => ({ default: m.PairDeviceDialog })))
const ProjectsView = lazyView(() => import("@/projects").then((m) => ({ default: m.ProjectsView })))
const ProjectView = lazyView(() => import("@/projects").then((m) => ({ default: m.ProjectView })))
const NewProjectDialog = lazyView(() => import("@/projects").then((m) => ({ default: m.NewProjectDialog })))
const NewWorkspaceDialog = lazyView(() => import("@/projects").then((m) => ({ default: m.NewWorkspaceDialog })))
const Unwired = lazyView(() => import("@/unwired").then((m) => ({ default: m.Unwired })))
const ChannelsRoute = lazyView(() => import("@/channels/channels").then((m) => ({ default: m.ChannelsRoute })))
const IntegrationsRoute = lazyView(() => import("@/integrations").then((m) => ({ default: m.IntegrationsRoute })))
const GitHubView = lazyView(() => import("@/github/index").then((m) => ({ default: m.GitHubView })))
const KnowledgeView = lazyView(() => import("@/knowledge").then((m) => ({ default: m.KnowledgeView })))
const SignalsView = lazyView(() => import("@/signals").then((m) => ({ default: m.SignalsView })))
const AutoRoute = lazyView(() => import("@/auto/automation").then((m) => ({ default: m.AutoRoute })))
const AccountsRoute = lazyView(() => import("@/accounts").then((m) => ({ default: m.AccountsRoute })))
const MailView = lazyView(() => import("@/mail/list").then((m) => ({ default: m.MailView }))); const ThreadView = lazyView(() => import("@/mail/thread").then((m) => ({ default: m.ThreadView })))
const VaultRoute = lazyView(() => import("@/vault").then((m) => ({ default: m.VaultRoute })))
const ContactsView = lazyView(() => import("@/people/routes").then((m) => ({ default: m.ContactsView })))
const ContactView = lazyView(() => import("@/people/routes").then((m) => ({ default: m.ContactView })))
const MeView = lazyView(() => import("@/people/routes").then((m) => ({ default: m.MeView })))
const OrgView = lazyView(() => import("@/people/routes").then((m) => ({ default: m.OrgView })))
const TeamView = lazyView(() => import("@/people/routes").then((m) => ({ default: m.TeamView })))
import { Robot } from "@/robot"
import { HoldBanner } from "@/queue"
import { YoloDialog } from "@/yolo/switch"
const YoloDigestView = lazyView(() => import("@/yolo/digest").then((m) => ({ default: m.YoloDigestView })))
const StatsRoute = lazyView(() => import("@/stats/page").then((m) => ({ default: m.StatsRoute })))
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
  const [q, setQ] = useState(""), t = useT()
  const parties = useParties(open ? q : "")
  useEffect(() => {
    const h = (e: KeyboardEvent) => { if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") { e.preventDefault(); open ? closeSheet() : openSheet("command") } }
    window.addEventListener("keydown", h); return () => window.removeEventListener("keydown", h)
  }, [open])
  const run = (f: () => void) => { closeSheet(); f() }
  const pages: [string, string, any][] = [["", t("shell:cmd.page.chat"), MessageCircleIcon], ["inbox", t("shell:cmd.page.inbox"), InboxIcon], ["items", t("shell:cmd.page.items"), ListTodoIcon], ["projects", t("shell:cmd.page.projects"), FolderIcon], ["sessions", t("shell:cmd.page.sessions"), SquareTerminalIcon], ["stats", t("shell:cmd.page.stats"), ChartNoAxesColumnIcon], ["signals", t("shell:cmd.page.signals"), RadioIcon], ["auto", t("shell:cmd.page.auto"), WorkflowIcon], ["integrations", t("shell:cmd.page.integrations"), PlugIcon], ["channels", t("shell:cmd.page.channels"), MessagesSquareIcon], ["contacts", t("shell:cmd.page.contacts"), ContactRoundIcon], ["knowledge", t("shell:cmd.page.knowledge"), BookOpenIcon], ["github", "GitHub", GitPullRequestIcon], ["accounts", t("shell:cmd.page.accounts"), KeyRoundIcon], ["vault", t("shell:cmd.page.vault"), LockIcon], ["bots", "Bot", BotIcon], ["devices", t("shell:cmd.page.devices"), ServerIcon], ["settings/general", t("shell:cmd.page.settings"), SettingsIcon], ["settings/access", t("shell:cmd.page.settingsAccess"), KeyRoundIcon], ["settings/tags", t("shell:cmd.page.settingsTags"), SettingsIcon], ["settings/workspace", t("shell:cmd.page.settingsSpace"), SettingsIcon]]
  return (
    <CommandDialog open={open} onOpenChange={(o) => !o && closeSheet()} title={t("common:action.search")} description={t("shell:cmd.description")}>
      <Command filter={(v, q) => (v.toLowerCase().includes(q.trim().toLowerCase()) ? 1 : 0)}>
      <CommandInput placeholder={t("shell:cmd.placeholder")} value={q} onValueChange={setQ} data-command-input />
      <CommandList className="max-h-[420px]">
        <CommandEmpty>{t("shell:cmd.empty")}</CommandEmpty>
        <CommandGroup heading={t("shell:cmd.actions")}>
          <CommandItem onSelect={() => run(() => { go("#/items"); openSheet("newItem") })}><PlusIcon />{t("shell:cmd.newItem")}<CommandShortcut>N</CommandShortcut></CommandItem>
          <CommandItem onSelect={() => run(() => { go("#/projects"); openSheet("newProject") })}><FolderIcon />{t("shell:cmd.newProject")}</CommandItem>
          <CommandItem onSelect={() => run(() => { go("#/bots"); openSheet("newBot") })} data-cmd-newbot><BotIcon />{t("shell:cmd.newBot")}</CommandItem>
          <CommandItem onSelect={() => run(() => openSheet("pairClient"))} data-cmd-pair-device><QrCodeIcon />{t("shell:cmd.pairDevice")}</CommandItem>
          <CommandItem onSelect={() => run(() => openSheet("hubs"))} data-cmd-switch-hub><ServerIcon />{t("shell:cmd.switchHub")}</CommandItem>
          <CommandItem onSelect={() => run(() => setTheme(resolvedTheme() === "dark" ? "light" : "dark"))}>{resolvedTheme() === "dark" ? <SunIcon /> : <MoonIcon />}{resolvedTheme() === "dark" ? t("shell:cmd.toLight") : t("shell:cmd.toDark")}</CommandItem>
        </CommandGroup>
        <CommandSeparator />
        <CommandGroup heading={t("shell:cmd.pages")}>{pages.map(([h, l, I]) => <CommandItem key={h} value={t("shell:cmd.pages") + " " + l} onSelect={() => run(() => go("#/" + h))}><I />{l}</CommandItem>)}</CommandGroup>
        <CommandSeparator />
        {parties.length > 0 && <><CommandGroup heading={t("shell:cmd.parties")}>{parties.map((p) => { const I = PARTY_ICON[p.type] ?? ContactRoundIcon; return <CommandItem key={p.type + p.id} value={t("shell:cmd.contact") + " " + p.id + " " + p.name + " " + q} onSelect={() => run(() => go(PARTY_HREF[p.type] + p.id))} data-cmd-party={p.id}><I /><span className="truncate">{p.name}</span>{p.sub && <CommandShortcut className="max-w-[45%] truncate">{p.sub}</CommandShortcut>}</CommandItem> })}</CommandGroup><CommandSeparator /></>}
        <CommandGroup heading={t("shell:cmd.page.projects")}>{st.projects.map((p) => <CommandItem key={p.id} value={t("shell:cmd.page.projects") + " " + p.name + " " + p.path} onSelect={() => run(() => go("#/projects/" + p.id))}><FolderIcon />{p.name}<CommandShortcut className="font-mono">{p.path.replace(/^\/Users\/[^/]+/, "~")}</CommandShortcut></CommandItem>)}</CommandGroup>
        <CommandSeparator />
        <CommandGroup heading="Bot">{Object.keys(st.bots).map((k) => <CommandItem key={k} value={"Bot " + st.bots[k].n + " " + st.bots[k].role} onSelect={() => run(() => go("#/bots/" + k))}><BotIcon />{st.bots[k].main ? st.bots[k].n : st.bots[k].n + " Bot"}<CommandShortcut>{st.bots[k].role.slice(0, 12)}</CommandShortcut></CommandItem>)}</CommandGroup>
        <CommandSeparator />
        <CommandGroup heading={t("shell:cmd.page.chat")}>{st.thread.filter((m) => m.k !== "thinking" && m.t).slice(-30).map((m, n) => <CommandItem key={n} value={t("shell:cmd.page.chat") + " " + m.t} onSelect={() => run(() => go("#/"))}><MessageCircleIcon /><span className="truncate">{m.t}</span></CommandItem>)}</CommandGroup>
        <CommandSeparator />
        <CommandGroup heading={t("shell:cmd.page.items")}>{st.items.map((i) => <CommandItem key={i.id} value={i.id + " " + i.t} onSelect={() => run(() => go("#/items/" + i.id))} data-cmd-item={i.id}><StatusDot s={i.st} /><ItemNo id={i.id} className="w-14" /><span className="truncate">{i.t}</span></CommandItem>)}</CommandGroup>
        <CommandSeparator />
        <CommandGroup heading={t("shell:cmd.page.sessions")}>{Object.keys(st.S).map((k) => { const s = st.S[k]; return <CommandItem key={k} value={t("shell:cmd.page.sessions") + " " + s.title + " " + (s.item ?? "")} onSelect={() => run(() => go("#/sessions/" + k))}><SquareTerminalIcon /><span className="truncate">{s.title}</span>{s.item && <CommandShortcut className="font-mono">{s.item} · #{s.n}</CommandShortcut>}</CommandItem> })}</CommandGroup>
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

/** signing in with a user's name and an access token (`steve-hub --access token create` on the Hub's machine) */
function TokenSignIn() {
  const t = useT()
  const [user, setUser] = useState("")
  const [token, setToken] = useState("")
  const [error, setError] = useState("")
  const [busy, setBusy] = useState(false)
  const go = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!user.trim() || !token.trim() || busy) return
    setBusy(true); setError("")
    try { await api.tokenSignIn(user.trim(), token.trim(), st.webVersion); location.reload() }
    catch (x) { setError((x as Error).message); setBusy(false) }
  }
  return (
    <form onSubmit={go} className="flex w-full max-w-xs flex-col gap-2 text-left" data-token-sign-in>
      <Input value={user} onChange={(e) => setUser(e.target.value)} placeholder={t("shell:app.userName")} autoComplete="username" autoFocus />
      <Input value={token} onChange={(e) => setToken(e.target.value)} placeholder={t("shell:app.accessToken")} type="password" autoComplete="current-password" />
      {error && <div className="type-footnote text-destructive">{error}</div>}
      <Button type="submit" disabled={busy || !user.trim() || !token.trim()}>{busy ? <Spinner className="size-4" /> : t("shell:app.signIn")}</Button>
      <div className="type-footnote text-muted-foreground">{t("shell:app.tokenHint")}</div>
    </form>
  )
}

function Splash({ children }: { children: React.ReactNode }) {
  return <div className="flex h-[100dvh] flex-col items-center justify-center gap-4 bg-background px-6 text-center text-foreground">{children}</div>
}
const UNWIRED = ["plugins"]

export default function App() {
  useStore()
  const t = useT()
  /* whether this page has a Hub to talk to: the one that served it, or the one it was pointed at */
  const [hub, setHubState] = useState<"checking" | "ok" | "missing">("checking")
  useEffect(() => {
    /* a browser keeps the Hub it is on in its list, so the list it takes to another Hub's page can lead back */
    const ok = (version: string) => { if (!nativeBridge()) rememberHub(currentHub(), { version }); setHubState("ok") }
    hello(hubBase()).then((h) => ok(h.version), async () => {
      /* served on its own port by a Hub (/hub.json): that Hub, when it answers, without asking */
      if (!hubBase()) {
        try {
          const j = await fetch("/hub.json", { cache: "no-store" }).then((r) => (r.ok ? r.json() : null))
          const h = typeof j?.hub === "string" ? await hello(j.hub) : null
          if (h?.api === API) { setHub(j.hub, { version: h.version }); setHubState("ok"); return }
        } catch { /* ask */ }
      }
      setHubState("missing")
    })
  }, [])
  useEffect(() => { if (hub === "ok") return start() }, [hub])
  const [a = "", b, c] = useRoute()
  /* an “撤销” toast belongs to the page it was raised on */
  useEffect(() => { sonnerToast.dismiss() }, [a, b, c])
  /* #/hub/add and #/hub/pair (a shell's menu, a link): the dialog is opened and the route left; before the page is
   * signed in, "add" is the list on the screen that is shown instead */
  const [intent, setIntent] = useState<"" | "add" | "pair">("")
  const [others, setOthers] = useState(false)
  const up = hub === "ok" && st.ready && !st.login
  useEffect(() => {
    if (a !== "hub") return
    setIntent(b === "pair" ? "pair" : "add")
    history.replaceState(null, "", location.pathname + location.search + "#/"); window.dispatchEvent(new HashChangeEvent("hashchange"))
  }, [a, b])
  useEffect(() => {
    if (!intent || !up) return
    openSheet(intent === "pair" ? "pairClient" : "hubs", { add: true }); setIntent("")
  }, [intent, up])
  const pairing = usePairRequest()
  const title = (sub: string) => <><Robot k="steve" size={72} /><div><div className="type-title">Steve</div><div className="mt-1 type-callout text-muted-foreground">{sub}</div></div></>
  const list = others || intent === "add"
  if (pairing) return <Splash><PairClaim /></Splash>
  if (hub === "missing") return (
    <Splash>{title(t("shell:app.connectTitle"))}<HubsOffer className="max-w-xs" /><HubChooser down className="max-w-xs" /></Splash>
  )
  if (st.login) return (
    <Splash>{title(t("shell:app.signInToContinue"))}<HubLine className="-mt-2" />
      {list ? <><HubChooser className="max-w-xs" /><Button variant="ghost" size="sm" onClick={() => { setOthers(false); setIntent("") }} data-login-back>{t("common:action.back")}</Button></> : <>
        {signIn.token && <TokenSignIn />}
        {(signIn.link || !signIn.token) && <Button variant={signIn.token ? "ghost" : "default"} onClick={() => { location.href = hubUrl(signIn.link ?? st.login ?? "/auth/login") + "?back=" + encodeURIComponent(hubBase() ? location.href : location.pathname + location.hash) }} data-login>{signIn.token ? t("shell:app.signInWithBrowser") : t("shell:app.signIn")}</Button>}
        <ScanButton />
        <Button variant="ghost" size="sm" onClick={() => setOthers(true)} data-login-change-hub>{t("shell:hub.other")}</Button></>}
      <HubsOffer className="max-w-xs" /></Splash>
  )
  if (!st.ready) return <Splash>{st.error ? <><div className="type-headline">{t("shell:app.cantConnect")}</div><HubLine className="-mt-2" /><div className="type-callout text-muted-foreground">{st.error}</div>
    {list ? <HubChooser down className="max-w-xs" /> : <div className="flex gap-2"><Button variant="outline" onClick={() => location.reload()}>{t("common:action.retry")}</Button><Button variant="ghost" onClick={() => setOthers(true)} data-error-change-hub>{t("shell:hub.other")}</Button></div>}
    <HubsOffer className="max-w-xs" /></> : <Spinner className="size-5 text-muted-foreground" />}</Splash>
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
        <SidebarInset className="relative min-w-0 overflow-hidden" key={(st.workspace?.id ?? "") + "/" + a + "/" + (b ?? "") + "/" + (c ?? "")}><HoldBanner /><div className="min-h-0 flex-1 anim-page">{view}</div></SidebarInset>
        <MobileTabs route={a} />
        {sh?.kind === "fork" && <ForkDialog k={sh.payload.k} at={sh.payload.at} />}
        {sh?.kind === "newItem" && <NewItemDialog project={sh.payload?.project} />}
        {sh?.kind === "newBot" && <NewBotDialog />}
        {sh?.kind === "graph" && <GraphDialog k={sh.payload.k} />}
        {sh?.kind === "link" && <LinkDialog k={sh.payload.k} />}
        {sh?.kind === "import" && <ImportDialog />}
        {sh?.kind === "deleteBot" && <DeleteBotDialog k={sh.payload.k} />}
        {sh?.kind === "pair" && <PairDialog />}
        {sh?.kind === "pairClient" && <PairDeviceDialog />}
        {sh?.kind === "hubs" && <HubsDialog add={sh.payload?.add} />}
        <HubsOfferDialog />
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
