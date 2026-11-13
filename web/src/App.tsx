import { lazyView } from "@/lazy"
import { ChatView } from "@/chat-view"
import { useEffect, useState } from "react"
import { Trans } from "react-i18next"
import { TooltipProvider } from "@/components/ui/tooltip"
import { Toaster, toast as notify } from "@/components/ui/toaster"
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Input } from "@/components/ui/input"
import { api, signIn } from "@/api"
import { API, currentHub, hello, hubBase, HubError, hubUrl, nativeBridge, openUrl, rememberHub, setHub } from "@/hub"
import { HubChooser, hubErrorText, HubLine, HubLostBanner, HubsDialog, HubsOffer, HubsOfferDialog, PairClaim, ScanButton, usePairRequest } from "@/hub-ui"
import { CommandPalette } from "@/command-palette"
import { st, useStore, start } from "@/store"
import { useT } from "@/i18n"
import { useRoute, getSheet, openSheet } from "@/ui"
import { Sidebar, MobileTabs } from "@/shell"
const InboxView = lazyView(() => import("@/views").then((m) => ({ default: m.InboxView })))
const ItemsView = lazyView(() => import("@/views").then((m) => ({ default: m.ItemsView })))
const ItemView = lazyView(() => import("@/views").then((m) => ({ default: m.ItemView })))
const SessionsView = lazyView(() => import("@/sessions-list").then((m) => ({ default: m.SessionsView })))
const SessionView = lazyView(() => import("@/views").then((m) => ({ default: m.SessionView })))
const MoreView = lazyView(() => import("@/views").then((m) => ({ default: m.MoreView })))
const DesignView = lazyView(() => import("@/views").then((m) => ({ default: m.DesignView })))
const DesignShowroom = lazyView(() => import("@/design-showroom").then((m) => ({ default: m.DesignShowroom })))
const ForkDialog = lazyView(() => import("@/views").then((m) => ({ default: m.ForkDialog })))
const NewItemDialog = lazyView(() => import("@/views").then((m) => ({ default: m.NewItemDialog })))
const GraphDialog = lazyView(() => import("@/views").then((m) => ({ default: m.GraphDialog })))
const LinkDialog = lazyView(() => import("@/views").then((m) => ({ default: m.LinkDialog })))
const ImportDialog = lazyView(() => import("@/views").then((m) => ({ default: m.ImportDialog })))
const BotsView = lazyView(() => import("@/manage").then((m) => ({ default: m.BotsView })))
const CapabilitiesView = lazyView(() => import("@/caps/page").then((m) => ({ default: m.CapabilitiesView })))
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
import { HoldBanner } from "@/queue"
import { YoloDialog } from "@/yolo/switch"
const YoloDigestView = lazyView(() => import("@/yolo/digest").then((m) => ({ default: m.YoloDigestView })))
const StatsRoute = lazyView(() => import("@/stats/page").then((m) => ({ default: m.StatsRoute })))


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
      <div className="type-footnote text-muted-foreground"><Trans t={t} i18nKey="shell:app.tokenHint" components={{ c: <code className="type-code wrap-anywhere text-foreground/80" /> }} /></div>
    </form>
  )
}

function Splash({ children }: { children: React.ReactNode }) {
  return <div className="flex h-[100dvh] flex-col items-center justify-center gap-4 bg-background px-6 text-center text-foreground">{children}</div>
}

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
  useEffect(() => { notify.dismiss() }, [a, b, c])
  /* #/hub/add and #/hub/pair (a shell's menu, a link): the dialog is opened and the route left; before the page is
   * signed in, "add" is the list on the screen that is shown instead */
  const [intent, setIntent] = useState<"" | "add" | "pair">("")
  const [others, setOthers] = useState(false)
  const up = hub === "ok" && st.ready && !st.login
  /* theme plugins installed on the Hub become choices in 外观 (plugin-themes.ts) once signed in */
  useEffect(() => { if (up) void syncPluginThemes() }, [up])
  useEffect(() => {
    if (a !== "hub") return
    setIntent(b === "pair" ? "pair" : "add")
    history.replaceState(null, "", location.pathname + location.search + "#/"); window.dispatchEvent(new HashChangeEvent("hashchange"))
  }, [a, b])
  useEffect(() => {
    if (!intent || !up) return
    /* in a browser anyone's link can carry the route: the code is made on a click. In a shell it is the shell's menu */
    if (intent === "pair") openSheet("pairClient", { ask: !nativeBridge() }); else openSheet("hubs", { add: true })
    setIntent("")
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
        {(signIn.link || !signIn.token) && !(signIn.refused && signIn.token) && <Button variant={signIn.token ? "ghost" : "default"} onClick={() => { location.href = hubUrl(signIn.link ?? st.login ?? "/auth/login") + "?back=" + encodeURIComponent(hubBase() ? location.href : location.pathname + location.hash) }} data-login>{signIn.token ? t("shell:app.signInWithBrowser") : t("shell:app.signIn")}</Button>}
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
    a === "capabilities" ? <CapabilitiesView tab={b} /> :
    a === "design" ? (b === "components" ? <DesignShowroom /> : <DesignView />) :
    a.split("?")[0] === "knowledge" ? <KnowledgeView sub={b} id={c} /> :
    a === "devices" ? (b ? <DeviceView key={b} id={b} /> : <DevicesView />) :
    a.split("?")[0] === "github" ? <GitHubView repo={b?.split("?")[0]} /> :
    a === "integrations" ? <IntegrationsRoute id={b} /> :
    a === "channels" ? <ChannelsRoute b={b} id={c} /> :
    a === "auto" && (b === "acc" || b === "space") ? <Redirect to={`#/channels/${b}/${c ?? ""}`} /> : a === "personas" ? <Redirect to={"#/channels/personas" + (b ? "/" + b : "")} /> : a === "plugins" ? <Redirect to="#/settings/plugins" /> :
    a.split("?")[0] === "signals" ? <SignalsView /> : a === "auto" ? <AutoRoute b={b} c={c} /> :
    a === "mail" || a.startsWith("mail?") ? (b === "t" && c ? <ThreadView key={c} id={c} /> : <MailView />) :
    a === "accounts" || a.startsWith("accounts?") ? <AccountsRoute route={a} id={b} sub={c} /> :
    a === "vault" ? <VaultRoute id={b} /> :
    a === "yolo" ? <YoloDigestView window={b} /> :
    a === "stats" ? <StatsRoute b={b} c={c} /> :
    a === "contacts" ? (b === "me" ? <MeView /> : b ? <ContactView key={b} id={b} /> : <ContactsView />) : a === "orgs" ? (b ? <OrgView key={b} id={b} /> : <ContactsView />) : a === "teams" ? (b ? <TeamView key={b} id={b} /> : <ContactsView />) :
    a === "more" ? <MoreView /> : a === "c" && b ? <ChatView key={b} conv={decodeURIComponent(b)} /> : <ChatView />
  const sh = getSheet()
  return (
    <TooltipProvider delay={400}>
      <SidebarProvider className="h-[100dvh] min-h-0! bg-background text-foreground [--sidebar-width:15rem]">
        <Sidebar route={a} />
        <SidebarInset className="relative min-w-0 overflow-hidden [view-transition-name:page]" key={(st.workspace?.id ?? "") + "/" + a + "/" + (b ?? "") + "/" + (c ?? "")}><HubLostBanner /><HoldBanner /><div className="min-h-0 flex-1">{view}</div></SidebarInset>
        <MobileTabs route={a} />
        {sh?.kind === "fork" && <ForkDialog k={sh.payload.k} at={sh.payload.at} />}
        {sh?.kind === "newItem" && <NewItemDialog project={sh.payload?.project} />}
        {sh?.kind === "newBot" && <NewBotDialog />}
        {sh?.kind === "graph" && <GraphDialog k={sh.payload.k} />}
        {sh?.kind === "link" && <LinkDialog k={sh.payload.k} />}
        {sh?.kind === "import" && <ImportDialog />}
        {sh?.kind === "deleteBot" && <DeleteBotDialog k={sh.payload.k} />}
        {sh?.kind === "pair" && <PairDialog />}
        {sh?.kind === "pairClient" && <PairDeviceDialog ask={sh.payload?.ask === true} />}
        {sh?.kind === "hubs" && <HubsDialog add={sh.payload?.add} />}
        <HubsOfferDialog />
        {sh?.kind === "tag" && <TagDialog id={sh.payload?.id} item={sh.payload?.item} name={sh.payload?.name} />}
        {sh?.kind === "newProject" && <NewProjectDialog />}
        {sh?.kind === "newWorkspace" && <NewWorkspaceDialog />}
        {sh?.kind === "yolo" && <YoloDialog />}
        <CommandPalette open={sh?.kind === "command"} />
        <Toaster />
      </SidebarProvider>
    </TooltipProvider>
  )
}
