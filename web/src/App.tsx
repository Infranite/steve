import { useNativeShell } from "@/use-native-shell"
import { lazyView } from "@/lazy"
import { useEffect, useState } from "react"
import { TooltipProvider } from "@/components/ui/tooltip"
import { Toaster, toast as notify } from "@/components/ui/toaster"
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Input } from "@/components/ui/input"
import { api, setPageWorkspace, signIn, Unauthenticated } from "@/api"
import { table } from "@/data/sync"
import { useTranscriptDemand } from "@/data/transcript"
import { deliveryWorkspace } from "@/delivery"
import { hubNavigationAllowed, API, currentHub, hello, hubBase, HubError, hubUrl, nativeBridge, openUrl, rememberHub, setHub } from "@/hub"
import { HubChooser, hubErrorText, HubLine, HubLostBanner, HubsDialog, HubsOffer, HubsOfferDialog, PairClaim, ScanButton, usePairRequest } from "@/hub-ui"
import { readHubInfo } from "@/hub-management-api"
import { getItem, st, useSession, useStoreSelector, start, type Sess } from "@/store"
import { lang, useT } from "@/i18n"
import { useCommandShortcut, useRoute, getSheet, openSheet } from "@/ui"
import { Sidebar, MobileTabs } from "@/shell"
const CommandPalette = lazyView(() => import("@/command-palette").then((m) => ({ default: m.CommandPalette })))
const FeedView = lazyView(() => import("@/feed").then((m) => ({ default: m.FeedView })))
const InboxView = lazyView(() => import("@/inbox").then((m) => ({ default: m.InboxView })))
const loadChat = () => import("@/chat-view")
const loadItems = () => import("@/items-list")
const ChatView = lazyView(() => loadChat().then((m) => ({ default: m.ChatView })))
const ItemsView = lazyView(() => loadItems().then((m) => ({ default: m.ItemsView })))
const ItemView = lazyView(() => import("@/views").then((m) => ({ default: m.ItemView })))
const SessionsView = lazyView(() => import("@/sessions-list").then((m) => ({ default: m.SessionsView })))
const loadSessionView = () => import("@/session/view")
const SessionView = lazyView(loadSessionView)
const MoreView = lazyView(() => import("@/views").then((m) => ({ default: m.MoreView })))
const MyDeliveriesView = lazyView(() => import("@/my-deliveries").then((m) => ({ default: m.MyDeliveriesView })))
const DesignView = lazyView(() => import("@/views").then((m) => ({ default: m.DesignView })))
const DesignShowroom = lazyView(() => import("@/design-showroom").then((m) => ({ default: m.DesignShowroom })))
const ForkDialog = lazyView(() => import("@/views").then((m) => ({ default: m.ForkDialog })))
const HandoffDialog = lazyView(() => import("@/views").then((m) => ({ default: m.HandoffDialog })))
const NewItemDialog = lazyView(() => import("@/views").then((m) => ({ default: m.NewItemDialog })))
const GraphDialog = lazyView(() => import("@/views").then((m) => ({ default: m.GraphDialog })))
const LinkDialog = lazyView(() => import("@/views").then((m) => ({ default: m.LinkDialog })))
const ImportDialog = lazyView(() => import("@/views").then((m) => ({ default: m.ImportDialog })))
const BotsView = lazyView(() => import("@/manage").then((m) => ({ default: m.BotsView })))
const CapabilitiesView = lazyView(() => import("@/caps/page").then((m) => ({ default: m.CapabilitiesView })))
const BotView = lazyView(() => import("@/manage").then((m) => ({ default: m.BotView })))
const NewBotDialog = lazyView(() => import("@/manage").then((m) => ({ default: m.NewBotDialog })))
const DeleteBotDialog = lazyView(() => import("@/manage").then((m) => ({ default: m.DeleteBotDialog })))
const HubOfflinePage = lazyView(() => import("@/devices-hub").then(m => ({ default: m.HubOfflinePage })))
const SettingsView = lazyView(() => import("@/manage").then((m) => ({ default: m.SettingsView })))
const TagDialog = lazyView(() => import("@/manage").then((m) => ({ default: m.TagDialog })))
const DevicesRoute = lazyView(() => import("@/devices").then((m) => ({ default: m.DevicesRoute })))
const AddDeviceDialog = lazyView(() => import("@/devices-add").then((m) => ({ default: m.AddDeviceDialog })))
const ProjectsView = lazyView(() => import("@/projects").then((m) => ({ default: m.ProjectsView })))
const ProjectView = lazyView(() => import("@/projects").then((m) => ({ default: m.ProjectView })))
const NewProjectDialog = lazyView(() => import("@/projects").then((m) => ({ default: m.NewProjectDialog })))
const NewWorkspaceDialog = lazyView(() => import("@/projects").then((m) => ({ default: m.NewWorkspaceDialog })))
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
import { syncPluginThemes } from "@/plugin-themes"
import { HoldBanner } from "@/hold-banner"
const StatsRoute = lazyView(() => import("@/stats/page").then((m) => ({ default: m.StatsRoute })))
const TodosView = lazyView(() => import("@/todos/page").then((m) => ({ default: m.TodosView })))
const TodoRoute = lazyView(() => import("@/todos/page").then((m) => ({ default: m.TodoRoute })))
const CalendarView = lazyView(() => import("@/calendar/page").then((m) => ({ default: m.CalendarView })))
const DeliveryPage = lazyView(() => import("@/delivery-page").then((m) => ({ default: m.DeliveryPage })))


/** signing in with a user's name and an access token (`steve-hub --access token create` on the Hub's machine) */
function TokenSignIn() {
  const t = useT()
  const [user, setUser] = useState("")
  const [token, setToken] = useState("")
  const [error, setError] = useState("")
  /* the browser did not keep the sign-in of a Hub on another address: that Hub's own page can */
  const [there, setThere] = useState(false)
  const [busy, setBusy] = useState(false)
  const go = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!user.trim() || !token.trim() || busy) return
    setBusy(true); setError(""); setThere(false)
    try { await api.tokenSignIn(user.trim(), token.trim(), st.webVersion); location.reload() }
    catch (x) { setError(hubErrorText(x)); setThere(x instanceof HubError && x.code === "no-session" && !!hubBase()); setBusy(false) }
  }
  return (
    <form onSubmit={go} className="flex w-full max-w-xs flex-col gap-2 text-left" data-token-sign-in>
      <Input value={user} onChange={(e) => setUser(e.target.value)} placeholder={t("shell:app.userName")} autoComplete="username" autoFocus />
      <Input value={token} onChange={(e) => setToken(e.target.value)} placeholder={t("shell:app.accessToken")} type="password" autoComplete="current-password" />
      {error && <div className="type-footnote text-destructive" role="alert" data-sign-in-error>{error}</div>}
      {there && <Button type="button" variant="outline" onClick={() => { location.href = openUrl(hubBase()) }} data-sign-in-there>{t("shell:hub.pairOpenThere")}</Button>}
      <Button type="submit" disabled={busy || !user.trim() || !token.trim()}>{busy ? <Spinner className="size-4" /> : t("shell:app.signIn")}</Button>
    </form>
  )
}

function Splash({ children }: { children: React.ReactNode }) {
  return <div className="flex h-[100dvh] flex-col items-center justify-center gap-4 bg-background px-6 text-center text-foreground">{children}</div>
}

export default function App({ deliveryId }: { deliveryId?: string }) {
  const [a = "", b, c] = useRoute()
  const inChat = a === "" || a === "c"
  useEffect(() => {
    if (deliveryId) return
    if (inChat) void loadChat().catch(() => {})
    else if (a === "items" && !b) void loadItems().catch(() => {})
    else if (a === "sessions" && b) void loadSessionView().catch(() => {})
  }, [a, b, inChat, deliveryId])
  const workspace = useStoreSelector(() => st.workspace)
  const ready = useStoreSelector(() => st.ready)
  const error = useStoreSelector(() => st.error)
  const signInPath = useStoreSelector(() => st.login)
  const itemTitle = useStoreSelector(() => a === "items" && b ? getItem(b)?.t : undefined)
  const projectTitle = useStoreSelector(() => a === "projects" && b ? st.projects.find((p) => p.id === b)?.name : undefined)
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
  const [deliveryPending, setDeliveryPending] = useState(!!deliveryId)
  const [deliveryError, setDeliveryError] = useState<unknown>(null)
  useEffect(() => {
    if (hub !== "ok") return
    if (!deliveryId) return start()
    let live = true, stop: (() => void) | undefined
    // Verify the delivery's ?ws= owner selector before loading this page's workspace views.
    void api.delivery(deliveryId).then((d) => {
      if (!live) return
      const ws = deliveryWorkspace(d, new URLSearchParams(location.search).get("ws"))
      if (!ws) throw new Error(t("pages:delivery.workspaceMissing"))
      setPageWorkspace(ws)
      setDeliveryPending(false)
      stop = start()
    }).catch((e) => { if (live) { setDeliveryError(e); setDeliveryPending(false) } })
    return () => { live = false; stop?.() }
  }, [hub, deliveryId, t])
  const login = deliveryError instanceof Unauthenticated ? deliveryError.login ?? "/auth/login" : signInPath
  const browserSignIn = async () => {
    const link = signIn.link ?? login ?? "/auth/login"
    // The loopback development login always redirects to /. Set its cookie on the user's click, then
    // reload this standalone URL; OIDC keeps its normal browser redirect and server-validated back target.
    if (deliveryId && link === "/auth/dev" && !hubBase()) {
      try {
        const r = await fetch(hubUrl(link), { credentials: "include" })
        if (r.ok) { location.reload(); return }
      } catch { /* the normal login route below can show its own error */ }
    }
    location.href = hubUrl(link) + "?back=" + encodeURIComponent(hubBase() ? location.href : location.pathname + location.search + location.hash)
  }
  useEffect(() => {
    let previousHash = location.hash
    const route = (e: HashChangeEvent) => {
      if (!hubNavigationAllowed()) { e.stopImmediatePropagation(); history.replaceState(null, "", previousHash); return }
      previousHash = location.hash
    }
    window.addEventListener("hashchange", route, true)
    return () => { window.removeEventListener("hashchange", route, true) }
  }, [])
  /* an “撤销” toast belongs to the page it was raised on */
  useEffect(() => { notify.dismiss() }, [a, b, c])
  /* #/hub/add and #/hub/pair (a shell's menu, a link): the dialog is opened and the route left; before the page is
   * signed in, "add" is the list on the screen that is shown instead */
  const [intent, setIntent] = useState<"" | "add" | "pair">("")
  const [others, setOthers] = useState(false)
  const up = hub === "ok" && !deliveryPending && !deliveryError && ready && !login
  // Start both reads once the workspace is bound, while the route's components load.
  const sessionId = up && a === "sessions" ? b : undefined
  const session = useSession(sessionId)
  useTranscriptDemand(sessionId && !table().tomb.has(`session:${sessionId}`) ? [sessionId] : [])
  /* theme plugins installed on the Hub become choices in 外观 (plugin-themes.ts) once signed in */
  useEffect(() => { if (up) void syncPluginThemes().catch((e) => console.warn(e)) }, [up])
  const base = hubBase()
  useEffect(() => {
    if (!up || deliveryId) return
    let pending = false
    const read = () => {
      if (pending || document.hidden) return
      pending = true
      void readHubInfo().catch(() => {}).finally(() => { pending = false })
    }
    read()
    window.addEventListener("focus", read)
    window.addEventListener("online", read)
    return () => { window.removeEventListener("focus", read); window.removeEventListener("online", read) }
  }, [up, base, deliveryId])
  useCommandShortcut(up && !deliveryId)
  useEffect(() => {
    if (a !== "hub") return
    setIntent(b === "pair" ? "pair" : "add")
    history.replaceState(null, "", location.pathname + location.search + "#/"); window.dispatchEvent(new HashChangeEvent("hashchange"))
  }, [a, b])
  useEffect(() => {
    if (!intent || !up) return
    /* in a browser anyone's link can carry the route: the code is made on a click. In a shell it is the shell's menu */
    if (intent === "pair") openSheet("addDevice", { kind: "client", ask: !nativeBridge() }); else openSheet("hubs", { add: true })
    setIntent("")
  }, [intent, up])
  const pairing = usePairRequest()
  const root = a.split("?")[0]
  const label = root === "" || root === "c" ? t("core:nav.chat")
    : root === "more" ? t("core:nav.mine")
    : t("core:nav." + root, { defaultValue: root === "github" ? "GitHub" : "Steve" })
  const pageTitle = root === "items" && b ? itemTitle ?? label
    : root === "projects" && b ? projectTitle ?? label
    : root === "sessions" && b ? session?.title ?? label : label
  useNativeShell({
    workspaceID: !login ? workspace?.id ?? null : null,
    workspaceName: !login ? workspace?.name ?? null : null,
    route: location.hash || "#/", title: String(pageTitle).slice(0, 512), language: lang(),
    ready: !pairing && (up || !!login || hub === "missing"), authenticated: !login && hub === "ok" && !!workspace,
  }, !!login && (others || intent === "add") || hub === "missing")
  /* the sign-in page shows Steve's live face, whose eyes follow the pointer (it keeps still under reduced motion); the other splash pages keep the static one */
  const title = (sub: string, live = false) => <><Robot k="steve" size={72} live={live} /><div><div className="type-title">Steve</div><div className="mt-1 type-callout text-muted-foreground">{sub}</div></div></>
  const list = others || intent === "add"
  if (pairing) return <Splash><PairClaim /></Splash>
  if ((a === "settings" || a === "devices") && b === "hub" && !signInPath && (hub === "missing" || (!ready && !!error))) return <HubOfflinePage />
  if (hub === "missing") return (
    <Splash>{title(t("shell:app.connectTitle"))}<HubsOffer className="max-w-xs" /><HubChooser down className="max-w-xs" /></Splash>
  )
  if (login) return (
    <Splash>{title(t("shell:app.signInToContinue"), true)}<HubLine className="-mt-2" />
      {list ? <><HubChooser className="max-w-xs" /><Button variant="ghost" size="sm" onClick={() => { setOthers(false); setIntent("") }} data-login-back>{t("common:action.back")}</Button></> : <>
        {signIn.token && <TokenSignIn />}
        {(signIn.link || !signIn.token) && !(signIn.refused && signIn.token) && <Button variant={signIn.token ? "ghost" : "default"} onClick={() => void browserSignIn()} data-login>{signIn.token ? t("shell:app.signInWithBrowser") : t("shell:app.signIn")}</Button>}
        <ScanButton />
        <Button variant="ghost" size="sm" onClick={() => setOthers(true)} data-login-change-hub>{t("shell:hub.other")}</Button></>}
      <HubsOffer className="max-w-xs" /></Splash>
  )
  if (deliveryError) return <Splash><div className="flex max-w-xl flex-col gap-3" role="alert" data-delivery-error>
    <h1 className="type-title">{t("pages:delivery.loadError")}</h1>
    <p className="type-callout text-muted-foreground">{t("pages:delivery.loadErrorHint")}</p>
    {deliveryError instanceof Error && <p className="wrap-anywhere type-footnote text-muted-foreground">{deliveryError.message}</p>}
    <Button className="self-center" variant="outline" onClick={() => location.reload()} data-delivery-retry>{t("common:action.retry")}</Button>
  </div></Splash>
  if (deliveryPending) return <Splash><Spinner className="size-5 text-muted-foreground" /></Splash>
  if (!ready) return <Splash>{error ? <><div className="type-headline">{t("shell:app.cantConnect")}</div><HubLine className="-mt-2" /><div className="type-callout text-muted-foreground">{error}</div>
    {list ? <HubChooser down className="max-w-xs" /> : <div className="flex gap-2"><Button variant="outline" onClick={() => location.reload()}>{t("common:action.retry")}</Button><Button variant="ghost" onClick={() => setOthers(true)} data-error-change-hub>{t("shell:hub.other")}</Button></div>}
    <HubsOffer className="max-w-xs" /></> : <Spinner className="size-5 text-muted-foreground" />}</Splash>
  if (deliveryId) return <DeliveryPage id={deliveryId} />
  return <WorkspacePage a={a} b={b} c={c} workspace={workspace?.id ?? ""} session={session} />
}

function WorkspacePage({ a, b, c, workspace, session }: { a: string; b?: string; c?: string; workspace: string; session?: Sess }) {
  const sh = useStoreSelector(getSheet)
  useStoreSelector(() => a === "" || a === "c" ? (sh ? st.ver : 0) : st.ver)
  const view =
    a.split("?")[0] === "feed" ? <FeedView workspace={workspace} filter={new URLSearchParams(a.split("?")[1]).get("kind") ?? ""} /> :
    a === "inbox" ? <InboxView /> :
    a.split("?")[0] === "todos" ? (b ? <TodoRoute key={b} id={b} /> : <TodosView query={a.split("?")[1]} />) :
    a.split("?")[0] === "calendar" ? <CalendarView query={(b === "e" ? c ?? "" : a).split("?")[1]} event={b === "e" && c ? decodeURIComponent(c.split("?")[0]) : undefined} /> :
    a === "items" ? (b ? <ItemView key={b} id={b} /> : <ItemsView />) :
    a === "projects" ? (b ? <ProjectView key={b} id={b} /> : <ProjectsView />) :
    a === "sessions" ? (b ? <SessionView key={b} k={b} s={session} /> : <SessionsView />) :
    /* Hub management and access moved to the devices pages */
    a === "settings" ? <SettingsView /> :
    a === "bots" ? (b ? <BotView key={b} k={b} /> : <BotsView />) :
    a === "capabilities" ? <CapabilitiesView tab={b} /> :
    a === "design" ? (b === "components" ? <DesignShowroom /> : <DesignView />) :
    a.split("?")[0] === "knowledge" ? <KnowledgeView sub={b} id={c} /> :
    a === "devices" ? <DevicesRoute b={b} c={c} /> :
    a.split("?")[0] === "github" ? <GitHubView repo={b?.split("?")[0]} /> :
    a === "integrations" ? <IntegrationsRoute id={b} /> :
    a === "channels" ? <ChannelsRoute b={b} id={c} /> :
    a.split("?")[0] === "signals" ? <SignalsView /> : a === "auto" ? <AutoRoute b={b} c={c} /> :
    a === "mail" || a.startsWith("mail?") ? (b === "t" && c ? <ThreadView key={c} id={c} /> : <MailView />) :
    a === "accounts" || a.startsWith("accounts?") ? <AccountsRoute route={a} id={b} sub={c} /> :
    a === "vault" ? <VaultRoute id={b} /> :
    a === "stats" ? <StatsRoute b={b} c={c} /> :
    a === "contacts" ? (b === "me" ? <MeView /> : b ? <ContactView key={b} id={b} /> : <ContactsView />) : a === "orgs" ? (b ? <OrgView key={b} id={b} /> : <ContactsView />) : a === "teams" ? (b ? <TeamView key={b} id={b} /> : <ContactsView />) :
    a === "publications" ? <MyDeliveriesView /> :
    a === "more" ? <MoreView /> : a === "c" && b ? <ChatView key={b} conv={decodeURIComponent(b)} /> : <ChatView />
  return (
    <TooltipProvider delay={400}>
      <SidebarProvider className="h-[100dvh] min-h-0! bg-background text-foreground [--sidebar-width:15rem]">
        <Sidebar route={a} />
        <SidebarInset className="relative min-w-0 overflow-hidden [view-transition-name:page]" key={currentHub() + "/" + workspace + "/" + a + "/" + (b ?? "") + "/" + (c ?? "")}><HubLostBanner /><HoldBanner /><div className="min-h-0 flex-1">{view}</div></SidebarInset>
        <MobileTabs route={a} />
        {sh?.kind === "fork" && <ForkDialog k={sh.payload.k} at={sh.payload.at} />}
        {sh?.kind === "handoff" && <HandoffDialog item={sh.payload.item} session={sh.payload.session} />}
        {sh?.kind === "newItem" && <NewItemDialog project={sh.payload?.project} bot={sh.payload?.bot} />}
        {sh?.kind === "newBot" && <NewBotDialog />}
        {sh?.kind === "graph" && <GraphDialog k={sh.payload.k} />}
        {sh?.kind === "link" && <LinkDialog k={sh.payload.k} />}
        {sh?.kind === "import" && <ImportDialog />}
        {sh?.kind === "deleteBot" && <DeleteBotDialog k={sh.payload.k} />}
        {sh?.kind === "addDevice" && <AddDeviceDialog key={JSON.stringify(sh.payload ?? {})} {...(sh.payload ?? {})} />}
        {sh?.kind === "hubs" && <HubsDialog add={sh.payload?.add} />}
        <HubsOfferDialog />
        {sh?.kind === "tag" && <TagDialog id={sh.payload?.id} item={sh.payload?.item} name={sh.payload?.name} />}
        {sh?.kind === "newProject" && <NewProjectDialog />}
        {sh?.kind === "newWorkspace" && <NewWorkspaceDialog />}
        {sh?.kind === "command" && <CommandPalette />}
        <Toaster />
      </SidebarProvider>
    </TooltipProvider>
  )
}
